use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("データベースのエラー: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("入出力のエラー: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip のエラー: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("JSON のエラー: {0}")]
    Json(#[from] serde_json::Error),
    #[error("見つかりません: {0}")]
    NotFound(String),
    #[error("入力が正しくありません: {0}")]
    Invalid(String),
    #[error("ワークスペースを自分自身やそのサブワークスペースの中には移動できません")]
    Cycle,
    #[error("対応していないエクスポート形式です（format_version = {0}）")]
    UnsupportedFormat(u32),
}
