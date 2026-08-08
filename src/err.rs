use actix_web::{HttpResponse, ResponseError};
#[cfg(any(feature = "master", feature = "chrome", feature = "agent-browser"))]
use image::ImageError;
#[cfg(any(feature = "master", feature = "chrome", feature = "agent-browser"))]
use qrcode::types::QrError;
use thiserror::Error;
use tracing::info;

#[derive(Error, Debug)]
pub enum CustomError {
    #[error("OtherLibraryError: {0}")]
    OtherLibraryError(String),
    #[cfg(feature = "typst")]
    #[error("JsonError: {0}")]
    JsonError(#[from] serde_json::Error),
    #[cfg(any(feature = "master", feature = "chrome", feature = "agent-browser"))]
    #[error("QrError: {0}")]
    QrError(#[from] QrError),
    #[cfg(any(feature = "master", feature = "chrome", feature = "agent-browser"))]
    #[error("ImageError: {0}")]
    ImageError(#[from] ImageError),
    #[error("IOError: {0}")]
    IOError(#[from] std::io::Error),
    #[cfg(any(feature = "master", feature = "chrome", feature = "agent-browser"))]
    #[error("TeraError: {0}")]
    TeraError(#[from] tera::Error),
    #[cfg(any(feature = "master", feature = "chrome"))]
    #[error("AnyhowError: {0}")]
    AnyhowError(#[from] anyhow::Error),
    #[cfg(any(feature = "master", feature = "typst", feature = "chrome"))]
    #[error("打印程序未找到！")]
    PrinterNoFound,
}
impl ResponseError for CustomError {
    fn error_response(&self) -> HttpResponse {
        match self {
            CustomError::OtherLibraryError(msg) => HttpResponse::InternalServerError().json(msg),
            #[cfg(any(feature = "master", feature = "chrome", feature = "agent-browser"))]
            CustomError::QrError(_) => HttpResponse::BadRequest().finish(),
            _ => {
                info!("{}", self);
                HttpResponse::InternalServerError().body(format!("error:{}", self))
            }
        }
    }
}
