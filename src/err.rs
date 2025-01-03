use actix_web::{HttpResponse, ResponseError};
use image::ImageError;
use qrcode::types::QrError;
use thiserror::Error;
use tracing::info;

#[derive(Error, Debug)]
pub enum CustomError {
    #[error("OtherLibraryError: {0}")]
    OtherLibraryError(String),
    #[error("QrError: {0}")]
    QrError(#[from] QrError),
    #[error("ImageError: {0}")]
    ImageError(#[from] ImageError),
    #[error("IOError: {0}")]
    IOError(#[from] std::io::Error),
    #[error("TeraError: {0}")]
    TeraError(#[from] tera::Error),
    #[error("AnyhowError: {0}")]
    AnyhowError(#[from] anyhow::Error),
    #[error("打印程序未找到！")]
    PrinterNoFound,
}
impl ResponseError for CustomError {
    fn error_response(&self) -> HttpResponse {
        match self {
            CustomError::OtherLibraryError(msg) => HttpResponse::InternalServerError().json(msg),
            CustomError::QrError(_) => HttpResponse::BadRequest().finish(),
            _ => {
                info!("{}", self);
                HttpResponse::InternalServerError().body(format!("error:{}", self))
            }
        }
    }
}
