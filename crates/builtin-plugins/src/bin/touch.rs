use dg_lab_link_builtin_plugins::plugins::TouchPlugin;
use dg_lab_link_plugin_sdk::run_plugin;

#[tokio::main]
async fn main() {
    if let Err(error) = run_plugin(TouchPlugin::default()).await {
        eprintln!("{}: {}", error.code, error.message);
        std::process::exit(1);
    }
}
