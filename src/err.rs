use actix_web::{HttpResponse, ResponseError};
use image::ImageError;
use qrcode::types::QrError;
use thiserror::Error;
use tracing::info;

#[derive(Error, Debug)]
pub enum CustomError {
    #[error("Other library error: {0}")]
    OtherLibraryError(String),
    #[error("Conversion error: {0}")]
    QrError(#[from] QrError),
    #[error("Conversion error: {0}")]
    ImageError(#[from] ImageError),
    #[error("Conversion error: {0}")]
    IOError(#[from] std::io::Error),
    #[error("Conversion error: {0}")]
    TeraError(#[from] tera::Error),
    #[error("Conversion error: {0}")]
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
