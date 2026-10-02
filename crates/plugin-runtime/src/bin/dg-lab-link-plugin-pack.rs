use std::path::Path;

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 2 {
        eprintln!("用法：dg-lab-link-plugin-pack <插件目录> <输出.dglabplugin>");
        std::process::exit(2);
    }
    match dg_lab_link_plugin_runtime::package::pack_directory(
        Path::new(&arguments[0]),
        Path::new(&arguments[1]),
    ) {
        Ok(manifest) => println!(
            "{}",
            serde_json::json!({"id":manifest.id,"version":manifest.version,"package":arguments[1].to_string_lossy()})
        ),
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::to_string(&error).expect("error serialize")
            );
            std::process::exit(1);
        }
    }
}
