use clap::{Arg, ArgAction, Command, value_parser};
use clap_num::maybe_hex;
use exhume_body::{Body, BodySlice};
use exhume_exfat::ExFatFS;
use log::{error, info};
use serde_json::{Value, json};
use std::fs::File;
use std::io::Write;

fn main() {
    let matches = Command::new("exhume_exfat")
        .version("0.1.0")
        .author("ForensicXlab")
        .about("Exhume artifacts from an exFAT partition.")
        .arg(
            Arg::new("body")
                .short('b')
                .long("body")
                .value_parser(value_parser!(String))
                .required(true)
                .help("Path to the body to exhume."),
        )
        .arg(
            Arg::new("format")
                .short('f')
                .long("format")
                .value_parser(value_parser!(String))
                .required(false)
                .help("Body format: 'raw' or 'ewf'."),
        )
        .arg(
            Arg::new("offset")
                .short('o')
                .long("offset")
                .value_parser(maybe_hex::<u64>)
                .required(true)
                .help("The exFAT partition start offset (bytes, dec or hex)."),
        )
        .arg(
            Arg::new("size")
                .short('s')
                .long("size")
                .value_parser(maybe_hex::<u64>)
                .required(true)
                .help("The size of the exFAT partition in sectors (dec or hex)."),
        )
        .arg(
            Arg::new("super")
                .long("super")
                .action(ArgAction::SetTrue)
                .help("Display boot sector / BPB info."),
        )
        .arg(
            Arg::new("list")
                .short('L')
                .long("list")
                .action(ArgAction::SetTrue)
                .help("List root directory entries."),
        )
        .arg(
            Arg::new("path")
                .short('p')
                .long("path")
                .value_parser(value_parser!(String))
                .required(false)
                .help("Read a file by absolute path from root, e.g. '/DCIM/100MEDIA/VID.MP4'."),
        )
        .arg(
            Arg::new("json")
                .short('j')
                .long("json")
                .action(ArgAction::SetTrue)
                .help("Output JSON where applicable."),
        )
        .arg(
            Arg::new("log_level")
                .short('l')
                .long("log-level")
                .value_parser(["error", "warn", "info", "debug", "trace"])
                .default_value("info"),
        )
        .arg(
            Arg::new("inode")
                .short('i')
                .long("inode")
                .value_parser(maybe_hex::<u64>)
                .help("Display metadata for a fake inode number."),
        )
        .arg(
            Arg::new("dir_entry")
                .short('d')
                .long("dir_entry")
                .requires("inode")
                .action(ArgAction::SetTrue)
                .help("If --inode is a directory, list directory entries (ext-like)."),
        )
        .arg(
            Arg::new("dump")
                .long("dump")
                .requires("inode")
                .action(ArgAction::SetTrue)
                .help("When --inode is set, dump content to 'inode_<N>.bin'"),
        )
        .get_matches();

    // Logger
    let lvl = matches.get_one::<String>("log_level").unwrap();
    let filter = match lvl.as_str() {
        "error" => log::LevelFilter::Error,
        "warn" => log::LevelFilter::Warn,
        "info" => log::LevelFilter::Info,
        "debug" => log::LevelFilter::Debug,
        "trace" => log::LevelFilter::Trace,
        _ => log::LevelFilter::Info,
    };
    env_logger::Builder::new().filter_level(filter).init();

    let file_path = matches.get_one::<String>("body").unwrap();
    let auto = String::from("auto");
    let format = matches.get_one::<String>("format").unwrap_or(&auto);
    let offset = matches.get_one::<u64>("offset").unwrap();
    let size = matches.get_one::<u64>("size").unwrap();

    let show_super = matches.get_flag("super");
    let json_output = matches.get_flag("json");
    let list_root = matches.get_flag("list");
    let path_opt = matches.get_one::<String>("path").cloned();
    let inode_num = matches.get_one::<u64>("inode").copied().unwrap_or(0);
    let show_dir_entry = matches.get_flag("dir_entry");
    let dump_content = matches.get_flag("dump");
    let dump = matches.get_flag("dump");

    // Body / slice
    let mut body = Body::new(file_path.to_owned(), format);
    let partition_size = *size * body.get_sector_size() as u64;
    let mut slice = match BodySlice::new(&mut body, *offset, partition_size) {
        Ok(sl) => sl,
        Err(e) => {
            error!("Could not create BodySlice: {}", e);
            return;
        }
    };

    let mut fs = match ExFatFS::new(&mut slice) {
        Ok(v) => v,
        Err(e) => {
            error!("Couldn't open exFAT: {}", e);
            return;
        }
    };

    if list_root {
        match fs.list_root_with_inodes() {
            Ok(list) => {
                if json_output {
                    let arr: Vec<Value> = list
                        .into_iter()
                        .map(|(inode, r)| {
                            json!({
                                "inode": inode,
                                "name": r.name,
                                "attributes": r.attributes,
                                "first_cluster": r.first_cluster,
                                "size": r.size
                            })
                        })
                        .collect();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({ "root": arr })).unwrap()
                    );
                } else {
                    for (inode, f) in list {
                        println!(
                            "0x{:016x}  {:>10}  cluster {:>8}  {}",
                            inode, f.size, f.first_cluster, f.name
                        );
                    }
                }
            }
            Err(e) => error!("List failed: {}", e),
        }
    }

    if inode_num > 0 {
        match fs.get_inode(inode_num) {
            Ok(inode) => {
                if show_dir_entry {
                    if inode.is_dir() {
                        match fs.list_dir_inode(&inode) {
                            Ok(entries) => {
                                if json_output {
                                    let arr: Vec<Value> =
                                        entries.iter().map(|de| de.to_json()).collect();
                                    println!(
                                        "{}",
                                        serde_json::to_string_pretty(&json!({"dir_entries": arr}))
                                            .unwrap()
                                    );
                                } else {
                                    for de in entries {
                                        println!("{} / 0x{:x} {}", de.inode, de.file_type, de.name);
                                    }
                                }
                            }
                            Err(e) => error!("dir list failed: {}", e),
                        }
                    } else {
                        error!("requested --dir_entry but inode is not a directory");
                    }
                } else {
                    if json_output {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&inode.to_json()).unwrap()
                        );
                    } else {
                        println!("{}", inode.to_string());
                    }
                }

                if dump_content {
                    if inode.is_dir() {
                        error!("cannot dump directory");
                    } else {
                        match fs.read_inode(&inode) {
                            Ok(bytes) => {
                                let filename = format!("inode_{}.bin", inode_num);
                                match File::create(&filename) {
                                    Ok(mut f) => {
                                        let _ = f.write_all(&bytes);
                                        info!("wrote {} bytes to '{}'", bytes.len(), filename);
                                    }
                                    Err(e) => error!("{}", e),
                                }
                            }
                            Err(e) => error!("read_inode failed: {}", e),
                        }
                    }
                }
            }
            Err(e) => error!("cannot get inode {}: {}", inode_num, e),
        }
    }
    if show_super {
        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&fs.super_info_json()).unwrap()
            );
        } else {
            println!("{}", fs.bpb.to_string());
        }
    }

    if let Some(path) = path_opt {
        match fs.read_path(&path) {
            Ok(bytes) => {
                if dump {
                    let leaf = std::path::Path::new(&path)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("dump.bin");
                    let outname = format!("{}.bin", leaf);
                    match File::create(&outname) {
                        Ok(mut f) => {
                            let _ = f.write_all(&bytes);
                            info!("wrote {} bytes to '{}'", bytes.len(), outname);
                        }
                        Err(e) => error!("{}", e),
                    }
                } else {
                    println!("read {} bytes", bytes.len());
                }
            }
            Err(e) => error!("read_path failed: {}", e),
        }
    }
}
