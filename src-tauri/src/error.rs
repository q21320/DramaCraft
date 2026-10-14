//! 统一错误类型。
//!
//! 前后端边界上序列化为 `{ code, message }`，前端据此决定是弹提示还是引导用户去设置页。

use serde::ser::SerializeStruct;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Msg(String),

    #[error("数据库错误：{0}")]
    Db(#[from] rusqlite::Error),

    #[error("文件读写失败：{0}")]
    Io(#[from] std::io::Error),

    #[error("数据解析失败：{0}")]
    Json(#[from] serde_json::Error),

    #[error("找不到可执行文件 {0}，请到「设置」里手动指定路径")]
    ToolMissing(String),

    #[error("{0}")]
    Process(String),
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Msg(_) => "E_MSG",
            AppError::Db(_) => "E_DB",
            AppError::Io(_) => "E_IO",
            AppError::Json(_) => "E_JSON",
            AppError::ToolMissing(_) => "E_TOOL_MISSING",
            AppError::Process(_) => "E_PROCESS",
        }
    }
}

impl serde::Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("AppError", 2)?;
        state.serialize_field("code", self.code())?;
        state.serialize_field("message", &self.to_string())?;
        state.end()
    }
}

pub type AppResult<T> = Result<T, AppError>;
