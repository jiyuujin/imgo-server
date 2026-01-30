use actix_cors::Cors;
use actix_multipart::Multipart;
use actix_web::{post, App, HttpServer, HttpResponse, Responder};
use futures_util::{StreamExt, TryStreamExt};
use image::{DynamicImage, ImageFormat, ImageOutputFormat};
use std::io::Cursor;

#[post("/compress")]
async fn compress_handler(mut payload: Multipart) -> impl Responder {
    let mut image_bytes = Vec::new();

    // 1. ファイルをメモリに読み込む
    while let Ok(Some(mut field)) = payload.try_next().await {
        while let Ok(Some(chunk)) = field.try_next().await {
            image_bytes.extend_from_slice(&chunk);
        }
    }

    if image_bytes.is_empty() {
        return HttpResponse::BadRequest().body("No data");
    }

    // 2. 画像デコード (PNG or JPG or TIFF などを自動で判別)
    let Ok(mut img) = image::load_from_memory(&image_bytes) else {
        return HttpResponse::BadRequest().body("Decode error");
    };

    // 3. 透過 PNG 対策：アルファチャンネルがあれば白背景と合成
    if img.color().has_alpha() {
        let rgba = img.to_rgba8();
        // 背景を白 (255, 255, 255, 255) で作成
        let mut background = image::ImageBuffer::from_pixel(
            img.width(), 
            img.height(), 
            image::Rgba([255, 255, 255, 255])
        );

        // アルファブレンディング (透過を考慮して重ねる)
        image::imageops::overlay(&mut background, &rgba, 0, 0);
        img = DynamicImage::ImageRgba8(background);
    }

    // 4. リサイズ (短辺 1024px)
    let scaled = img.thumbnail(1024, 1024);

    // 5. JPG エンコード
    let mut buffer = Cursor::new(Vec::new());

    // 第 3 引数に品質 (1 - 100) を指定可能
    if scaled.write_to(&mut buffer, ImageFormat::Jpeg).is_err() {
        return HttpResponse::InternalServerError().body("Encode error");
    }

    HttpResponse::Ok()
        .content_type("image/jpeg")
        .body(buffer.into_inner())
}

#[post("/compress_webp")]
async fn compress_webp_handler(mut payload: Multipart) -> impl Responder {
    let mut image_bytes = Vec::new();

    // 1. ファイルをメモリに読み込む
    while let Ok(Some(mut field)) = payload.try_next().await {
        while let Ok(Some(chunk)) = field.try_next().await {
            image_bytes.extend_from_slice(&chunk);
        }
    }

    if image_bytes.is_empty() {
        return HttpResponse::BadRequest().body("No data");
    }

    // 2. 画像デコード (PNG or JPG or TIFF などを自動で判別)
    let Ok(img) = image::load_from_memory(&image_bytes) else {
        return HttpResponse::BadRequest().body("Decode error");
    };

    // 3. リサイズ処理 (共通)
    let scaled = img.thumbnail(1024, 1024);

    // 4. WebP エンコード
    let mut buffer = Cursor::new(Vec::new());

    // image クレートの WebP エンコーダーを使用
    if scaled.write_to(&mut buffer, ImageFormat::WebP).is_err() {
        return HttpResponse::InternalServerError().body("WebP Encode error");
    }

    HttpResponse::Ok()
        .content_type("image/webp")
        .body(buffer.into_inner())
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let port: u16 = port.parse().expect("PORT must be a number");

    HttpServer::new(|| {
        let cors = Cors::default()
            .allow_any_origin() // 利用環境に応じて設定する必要あり
            .allow_any_method()
            .allow_any_header();

        App::new()
            .wrap(cors)
            .service(compress_handler)
            .service(compress_webp_handler)
    })
    .bind(("0.0.0.0", port))? // Cloud Run 対応
    .run()
    .await
}
