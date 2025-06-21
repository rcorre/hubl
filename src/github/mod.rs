pub mod code;
pub mod issues;

#[derive(Clone)]
pub struct Github {
    pub host: String,
    pub token: String,
}
