#[derive(serde::Deserialize, serde::Serialize)]
pub struct TrainingJobArgs {
    // TODO: Currently optional to maintain backward compatibility, but should be required in the future
    /// The package name
    pub package: Option<String>,
    /// The function to run
    pub function: String,
    /// Config file path
    pub args: Option<serde_json::Value>,
}
