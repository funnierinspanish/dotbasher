use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::io::{self, BufRead};
use std::path::Path;
use std::vec;
use cliclack::{select, intro};
use console::style;

#[derive(PartialEq)]
enum ConflictType {
    WriteNew,
    CacheIncoming,
}

#[derive(Clone, Debug)]
enum AliasSource {
    Path(String),
    Default,
}

impl AliasSource {
    fn get_path(&self) -> String {
        match self {
            AliasSource::Path(path) => path.clone(),
            AliasSource::Default => "default".to_string(),
        }
    }
}

#[derive(Clone)]
struct Alias {
    value: String,
    source: AliasSource,
}

static mut IGNORE_FUTURE_CONFLICTS: bool = false;
static mut ACCEPT_ALL_NEW: bool = false;
static mut YOLO_MODE: bool = false;

const ALIAS_DIR: &str = "aliases";
const BASHRC: &str = ".bashrc";
const BASE_BASHRC: &str = "/etc/skel/.bashrc";
const MODULAR_ALIAS_START_MARKER: &str = "#--- BEGIN Modular Aliases [Block's contents will be replaced on build] ---";
const MODULAR_ALIAS_END_MARKER: &str = "#--- END Modular Aliases ---";

fn main() -> io::Result<()> {
    // Parse command-line arguments.
    let args: Vec<String> = env::args().collect();
    let auto_confirm = args.iter().any(|arg| arg == "--auto-confirm" || arg == "-y");
    let remove_aliases = args.iter().any(|arg| arg == "--remove-aliases");
    let mut bashrc_file_exists = false;

    // Load existing .bashrc content (use base template if not present).
    let bashrc_path = Path::new(BASHRC);
    let base_content = if bashrc_path.exists() {
        bashrc_file_exists = true;
        fs::read_to_string(BASHRC).expect("Failed to fail properly, what a fail.")
    } else {
        println!(".bashrc not found. Using base template from {}...", BASE_BASHRC);
        fs::read_to_string(BASE_BASHRC).expect("Failed to fail properly, what a fail.")
    };

    if bashrc_file_exists && remove_aliases {
        println!("Removing existing modular aliases...");
        let new_bashrc = remove_section(&base_content, MODULAR_ALIAS_START_MARKER, MODULAR_ALIAS_END_MARKER);
        fs::write(BASHRC, new_bashrc).expect("Failed to fail properly, what a fail.");
    
        return Ok(());
    };

    unsafe {
        YOLO_MODE = auto_confirm;
    }

    // Check aliases directory exists.
    if !Path::new(ALIAS_DIR).exists() {
        eprintln!("Error: '{}' directory not found. Please create it.", ALIAS_DIR);
        std::process::exit(1);
    }

    // Parse any existing modular aliases from .bashrc.
    let existing_aliases: HashMap<String, Alias> = if let Some(existing_section) = extract_section(&base_content, MODULAR_ALIAS_START_MARKER, MODULAR_ALIAS_END_MARKER) {
        parse_modular_aliases(existing_section, AliasSource::Default)
    } else {
        HashMap::new()
    };

    // Prepare a hash map for incoming alias definitions.
    let mut incoming_aliases: HashMap<String, Alias> = HashMap::new();
    
    // Prepare a hash map for referenced include file paths.
    let mut referenced_include_file_paths: Vec<String> = vec![];

    // Process additional alias files in the aliases directory.
    let mut entries: Vec<_> = fs::read_dir(PathBuf::from(ALIAS_DIR))?
        .map(|res| res.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, std::io::Error>>()?;

    entries.sort();


    // alias_files.sort_by(|a, b| a.cmp(&b));
    for entry in entries {
        let path = entry.as_path();
        if path.is_file() {
            let file_path = path.to_str().expect("Failed to fail properly, what a fail.");
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                if filename == env::args().next().unwrap_or_default() {
                    continue;
                }
                process_alias_file(file_path, &mut incoming_aliases, &mut referenced_include_file_paths).expect("Failed to fail properly, what a fail.");
            }
        }
    }

    // Merge incoming aliases with existing ones (if not removing).
    let new_aliases_from_file = compile_new_aliases(&existing_aliases, &incoming_aliases).expect("Failed to fail properly, what a fail.");

    // Build final modular alias section in a sorted order.
    let mut new_alias_block = String::new();
    new_alias_block.push_str(&format!("{}\n", MODULAR_ALIAS_START_MARKER));

    let mut sorted_aliases: Vec<String> = new_aliases_from_file.clone().into_iter().map(|(alias_name, _)| (alias_name)).collect();
    sorted_aliases.sort_by(|a, b| a.cmp(b));
    for alias_line in sorted_aliases {
        new_alias_block.push_str(&format!("alias {}={}", alias_line, new_aliases_from_file.get(&alias_line).unwrap().value));
        new_alias_block.push('\n');
    }
    new_alias_block.push_str(&format!("{}\n", MODULAR_ALIAS_END_MARKER));

    // Remove the existing modular alias section (if any) and append the new section.
    let new_bashrc = remove_section(&base_content, MODULAR_ALIAS_START_MARKER, MODULAR_ALIAS_END_MARKER);
    let final_bashrc = format!("{}{}", new_bashrc, new_alias_block);
    fs::write(BASHRC, final_bashrc).expect("Failed to fail properly, what a fail.");

    println!("Modular alias setup complete. Remember to source your .bashrc (e.g., 'source ~/.bashrc') to apply the changes.");
    Ok(())
}

/// Processes an alias file, inserting any lines starting with "alias " into the map.
fn process_alias_file(path: &str, alias_map: &mut HashMap<String, Alias>, referenced_include_file_paths: &mut Vec<String>) -> io::Result<()> {
    let file = fs::File::open(path).expect("Failed to fail properly, what a fail.");
    let reader = io::BufReader::new(file);
    for line_result in reader.lines() {
        let line = line_result?;
        let trimmed_line = line.trim();

        if trimmed_line.starts_with("alias ") {
            cache_incoming_aliases(&path, alias_map, &line).expect("Failed to insert alias.");
        } else if trimmed_line.starts_with("#include:") {
            let include_path = format!("{}/{}", ALIAS_DIR, trimmed_line[9..].trim());
            match process_includes_file_references(&include_path, referenced_include_file_paths) {
                Some(path) => {
                    process_alias_file(&path, alias_map, referenced_include_file_paths).expect("Failed to process included file.");
                },
                None => {
                    continue;
                }
            }
        }
    }
    Ok(())
}

fn process_includes_file_references(path: &str, referenced_include_file_paths: &mut Vec<String>) -> Option<String> {
    if Path::new(path).exists() {
        if referenced_include_file_paths.contains(&path.to_string()) {
            return None;
        } else {
            referenced_include_file_paths.push(path.to_string());
            return Some(path.to_string());
        }
    } else {
        eprintln!("Error: Included file not found: {}", path);
        return None;
    }
}

fn show_diff(alias: &str, old_val: Alias, new_val: Alias, conflict_type: &ConflictType) {
    let source_path = old_val.source.get_path();
    let dest_path = new_val.source.get_path();
    let mut source_name = source_path.to_string();
    let mut dest_name = dest_path.to_string(); 

    if conflict_type == &ConflictType::WriteNew {
        source_name = "*** Alias loader sources ***".to_string();
        dest_name = "*** .bashrc file ***".to_string();
    };
    
    let colored_diff_str = format!(
        "{}\n{}\n@@ -1 +1 @@\n {}\n {}\n",
        style(format!("--- {}", source_name.clone())).red(),
        style(format!("+++ {}", dest_name.clone())).green(),
        style(format!("-  {}", &old_val.value)).red(),
        style(format!("+  {}", &new_val.value)).green(),
    );


    intro(style("Conflict!").on_cyan().black()).expect("Failed to display intro.");
    cliclack::note(format!("Alias: {}", style(format!("{}", alias)).cyan()),
    colored_diff_str,
    ).expect("msg");

}

fn conflict_resolver(alias_name: &str, current_val: &Alias, new_val: &Alias, conflict_type: ConflictType) -> Option<Alias> {    
    let new_val_value = new_val.value.clone();

    if conflict_type == ConflictType::WriteNew {
        cliclack::note("Current value", current_val.clone().value).expect("Failed to print note.");
        cliclack::note("Incoming cached value", new_val_value.clone()).expect("Failed to print note.");
    } else {
        // cliclack::note(format!("Current cached value (from {:?})", current_val.source.get_path()), current_val.clone().value).expect("Failed to print note.");
        // cliclack::note(format!("Incoming value (from {:?})", new_val.source.get_path()), new_val.value.clone()).expect("Failed to print note.");
        show_diff(alias_name, current_val.clone(), new_val.clone(), &conflict_type);
    }

    let conflict_resolution_heading = if conflict_type == ConflictType::WriteNew {
        format!("Replace the contents of the new alias {} with {}", style(alias_name).cyan().bold(), style(new_val_value.clone()).magenta())
    } else {   
        format!("Replace the new alias {} on your `.bashrc` file with with {}", style(alias_name).cyan().bold(), style(new_val_value.clone()).magenta())
    };
    
    let answer = select(conflict_resolution_heading)
        .item("y", "Yes", "Overwrite the existing alias. Default.")
        .item("n", "No", "Keep the current the value.")
        .item("i", "Ignore all", "Ignore all future conflicts.")
        .item("a", "Accept all new", "Accept all new changes for subsequent conflicts.") 
        .filter_mode()
        .interact()
        .expect("Failed to get valid answer.");

    match answer.trim().to_lowercase().as_str() {
        "y" => {
            // Overwrite with incoming alias.
            return Some(new_val.clone());
        },
        "n" => {
            // Keep the existing alias.
            return None;
        },
        "i" => {
            // Ignore all future conflicts.
            unsafe {
                IGNORE_FUTURE_CONFLICTS = true;
            }
            return None;
        },
        "a" => {
            // Accept all new changes for subsequent conflicts.
            unsafe {
                ACCEPT_ALL_NEW = true;
            }
            return Some(new_val.clone());
        },
        _ => {
            // Default: Overwrite with incoming alias.
            return Some(new_val.clone());

        }
    }
}

/// Inserts an alias definition into the map; if a duplicate exists, prompts the user in interactive mode.
fn cache_incoming_aliases(path: &str, alias_map: &mut HashMap<String, Alias>, alias_line: &str) -> io::Result<()> {
    if let Some((alias_name, incoming_value)) = parse_alias_line(alias_line, AliasSource::Path(path.to_string())) {
        // Check if alias already exists.
        if let Some(pre_cached_value) = alias_map.get(&alias_name) {
            if pre_cached_value.value != incoming_value.value {
                if unsafe { YOLO_MODE } {
                    alias_map.insert(alias_name, incoming_value);
                    return Ok(());
                } else {
                    // If global flags are already set, obey them.
                    unsafe {
                        if ACCEPT_ALL_NEW {
                            alias_map.insert(alias_name, incoming_value);
                            return Ok(());
                        }
                        if IGNORE_FUTURE_CONFLICTS {
                            return Ok(());
                        }
                    }

                    match conflict_resolver(&alias_name, &pre_cached_value, &incoming_value, ConflictType::CacheIncoming) {
                        Some(resolved_value) => {
                            alias_map.insert(alias_name, resolved_value);
                        },
                        None => {
                            return Ok(());
                        }
                    }
                }
            }
        } else {
            // No conflict; insert the alias.
            alias_map.insert(alias_name, incoming_value);
        }
    }
    Ok(())
}

/// Parses an alias line (starting with "alias ") to extract the alias name.
fn parse_alias_line(line: &str, alias_source: AliasSource) -> Option<(String, Alias)> {
    let trimmed = line.trim();
    if trimmed.starts_with("alias ") {
        let without_prefix = &trimmed[6..];
        return match without_prefix.split_once('=') {
            Some((alias_name, alias_value)) => Some((alias_name.trim().to_string(), Alias { value: alias_value.trim().to_string(), source: alias_source })),
            None => None
        };
    }
    None
}

/// Extracts the text between start_marker and end_marker from content.
fn extract_section<'a>(content: &'a str, start_marker: &str, end_marker: &str) -> Option<&'a str> {
    if let Some(start_index) = content.find(start_marker) {
        let remainder = &content[start_index..];
        if let Some(offset) = remainder.find(end_marker) {
            let end_index = start_index + offset + end_marker.len();
            return Some(&content[start_index..end_index]);
        }
    }
    None
}

/// Parses a modular alias section into a HashMap.
fn parse_modular_aliases(section: &str, alias_source: AliasSource) -> HashMap<String, Alias> {
    let mut map: HashMap<String, Alias> = HashMap::new();
    for line in section.lines() {
        if let Some((alias_name, alias)) = parse_alias_line(line, alias_source.clone()) {
            map.insert(alias_name, alias);
        }
    }
    map
}

/// Merges existing aliases with incoming ones. For conflicts, shows a diff and lets the user decide.
fn compile_new_aliases(
    old: &HashMap<String, Alias>,
    new: &HashMap<String, Alias>,
) -> io::Result<HashMap<String, Alias>> {
    let mut final_map = old.clone();

    for (alias_key, new_value) in new {
        if let Some(old_value) = old.get(alias_key) {
            if old_value.value != new_value.value {
                if unsafe { YOLO_MODE } {
                    if let Some(resolved_value) = conflict_resolver(alias_key, old_value, new_value, ConflictType::WriteNew) {
                        final_map.insert(alias_key.to_string(), resolved_value);
                    }
                } else {
                    final_map.insert(alias_key.to_string(), new_value.clone());
                }
            }
        } else {
            final_map.insert(alias_key.to_string(), new_value.clone());
        }
    }
    Ok(final_map)
}

/// Repeatedly removes all occurrences of the section between start_marker and end_marker from content.
fn remove_section(content: &str, start_marker: &str, end_marker: &str) -> String {
    let mut result = content.to_string();
    loop {
        if let Some(start) = result.find(start_marker) {
            if let Some(end) = result[start..].find(end_marker) {
                let end_index = start + end + end_marker.len();
                result = result[..start-1].to_string() + &result[end_index..];
            } else {
                break;
            }
        } else {
            break;
        }
    }
    result
}
