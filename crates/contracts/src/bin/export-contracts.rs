use dg_lab_link_contracts::{
    ControlCommand, hub::HubSnapshot, preferences::AppPreferencesSnapshot,
};
fn main() {
    println!(
        "{}",
        serde_json::json!({
            "ControlCommand": schemars::schema_for!(ControlCommand),
            "HubSnapshot": schemars::generate::SchemaSettings::draft2020_12().for_serialize().into_generator().into_root_schema_for::<HubSnapshot>(),
            "AppPreferencesSnapshot": schemars::generate::SchemaSettings::draft2020_12().for_serialize().into_generator().into_root_schema_for::<AppPreferencesSnapshot>(),
            "WaveformConfig": schemars::generate::SchemaSettings::draft2020_12().for_serialize().into_generator().into_root_schema_for::<dg_lab_link_contracts::sources::WaveformConfig>()
        })
    );
}
