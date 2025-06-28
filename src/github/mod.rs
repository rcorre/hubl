pub mod code;
pub mod issues;

use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Match {
    pub text: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct TextMatch {
    pub matches: Vec<Match>,
}

#[derive(Clone)]
pub struct Github {
    pub host: String,
    pub token: String,
}
