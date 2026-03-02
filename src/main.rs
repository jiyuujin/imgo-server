use actix_cors::Cors;
use actix_multipart::Multipart;
use actix_web::{post, App, HttpServer, HttpResponse, Responder, web};
use futures_util::TryStreamExt;
use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba};
use std::io::Cursor;

use ab_glyph::{FontArc, PxScale};
use imageproc::drawing::draw_text_mut;
use serde::Deserialize;

struct FontState {
    regular: FontArc,
    bold: FontArc,
    base_image: ImageBuffer<Rgba<u8>, Vec<u8>>,
}

fn draw_wrapped_text(
    canvas: &mut ImageBuffer<Rgba<u8>, Vec<u8>>,
    font: &FontArc,
    text: &str,
    x: i32,
    y: i32,
    scale: PxScale,
    color: Rgba<u8>,
    max_width: u32,
    line_height: i32,
) {
    let mut lines: Vec<String> = Vec::new();
    let mut current_line = String::new();
    let mut current_width = 0.0f32;

    for ch in text.chars() {
        let ch_width = if (ch as u32) > 0x7F { scale.x } else { scale.x * 0.55 };
        if current_width + ch_width > max_width as f32 {
            lines.push(current_line.clone());
            current_line = ch.to_string();
            current_width = ch_width;
        } else {
            current_line.push(ch);
            current_width += ch_width;
        }
    }
    if !current_line.is_empty() { lines.push(current_line); }

    for (i, line) in lines.iter().enumerate() {
        imageproc::drawing::draw_text_mut(
            canvas,
            color,
            x,
            y + (i as i32 * line_height),
            scale,
            font,
            line,
        );
    }
}

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

#[derive(Deserialize)]
pub struct OgpQuery {
    title: Option<String>,
    subtitle: Option<String>,
}

#[actix_web::get("/ogp")]
async fn ogp_get_handler(
    query: web::Query<OgpQuery>,
    state: web::Data<FontState>,
) -> impl Responder {
    let title = query.title.clone().unwrap_or_default();
    let subtitle = query.subtitle.clone().unwrap_or_default();

    let mut canvas = state.base_image.clone();

    // let mut canvas = ImageBuffer::from_pixel(
    //     1200, 630,
    //     Rgba([255, 255, 255, 255]),
    // );

    // for y in 380..630u32 {
    //     for x in 0..1200u32 {
    //         let ratio = (y - 380) as f32 / 250.0;
    //         let alpha = (ratio * 200.0) as u32;
    //         let pixel = canvas.get_pixel_mut(x, y);
    //         pixel[0] = (pixel[0] as u32 * (255 - alpha) / 255) as u8;
    //         pixel[1] = (pixel[1] as u32 * (255 - alpha) / 255) as u8;
    //         pixel[2] = (pixel[2] as u32 * (255 - alpha) / 255) as u8;
    //         pixel[3] = 255;
    //     }
    // }

    if !title.is_empty() {
        draw_wrapped_text(&mut canvas, &state.bold, &title, 60, 210, PxScale::from(64.0), Rgba([26, 26, 26, 255]), 1080, 76);
    }
    if !subtitle.is_empty() {
        draw_wrapped_text(&mut canvas, &state.regular, &subtitle, 60, 355, PxScale::from(36.0), Rgba([117, 117, 117, 255]), 1080, 44);
    }

    let mut buffer = Cursor::new(Vec::new());
    if DynamicImage::ImageRgba8(canvas)
        .write_to(&mut buffer, ImageFormat::Jpeg)
        .is_err()
    {
        return HttpResponse::InternalServerError().body("Encode error");
    }

    HttpResponse::Ok()
        .content_type("image/jpeg")
        .body(buffer.into_inner())
}

// #[post("/ogp")]
// async fn ogp_handler(
//     mut payload: Multipart,
//     state: web::Data<FontState>,
// ) -> impl Responder {
//     let mut image_bytes: Vec<u8> = Vec::new();
//     let mut title = String::new();
//     let mut subtitle = String::new();

//     while let Ok(Some(mut field)) = payload.try_next().await {
//         let field_name = field
//             .content_disposition()
//             .get_name()
//             .unwrap_or("")
//             .to_string();

//         let mut data = Vec::new();
//         while let Ok(Some(chunk)) = field.try_next().await {
//             data.extend_from_slice(&chunk);
//         }

//         match field_name.as_str() {
//             "image"    => image_bytes = data,
//             "title"    => title    = String::from_utf8_lossy(&data).to_string(),
//             "subtitle" => subtitle = String::from_utf8_lossy(&data).to_string(),
//             _ => {}
//         }
//     }

//     let base_img = if !image_bytes.is_empty() {
//         match image::load_from_memory(&image_bytes) {
//             Ok(img) => img,
//             Err(_)  => return HttpResponse::BadRequest().body("Base image decode error"),
//         }
//     } else {
//         DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
//             1200, 630,
//             Rgba([255, 255, 255, 255]),
//         ))
//     };

//     let base_img = base_img.resize_to_fill(
//         1200, 630,
//         FilterType::Lanczos3,
//     );

//     let mut canvas = base_img.to_rgba8();

//     for y in 380..630u32 {
//         for x in 0..1200u32 {
//             let alpha_factor = (y - 380) as f32 / 250.0;
//             let alpha = (alpha_factor * 200.0) as u32;
            
//             let pixel = canvas.get_pixel_mut(x, y);

//             pixel[0] = (pixel[0] as u32 * (255 - alpha) / 255) as u8;
//             pixel[1] = (pixel[1] as u32 * (255 - alpha) / 255) as u8;
//             pixel[2] = (pixel[2] as u32 * (255 - alpha) / 255) as u8;
//             pixel[3] = 255; // アルファは固定
//         }
//     }

//     if !title.is_empty() {
//         draw_wrapped_text(
//             &mut canvas,
//             &state.bold,
//             &title,
//             60, 430,
//             PxScale::from(64.0),
//             Rgba([255, 255, 255, 255]), // ここが Rgba<u8> であることを確認
//             1080, 76,
//         );
//     }

//     if !subtitle.is_empty() {
//         draw_wrapped_text(
//             &mut canvas,
//             &state.regular,
//             &subtitle,
//             60, 545,
//             PxScale::from(36.0),
//             Rgba([220, 220, 220, 255]),
//             1080, 44,
//         );
//     }

//     let mut buffer = Cursor::new(Vec::new());
//     if DynamicImage::ImageRgba8(canvas)
//         .write_to(&mut buffer, ImageFormat::Jpeg)
//         .is_err()
//     {
//         return HttpResponse::InternalServerError().body("Encode error");
//     }

//     HttpResponse::Ok()
//         .content_type("image/jpeg")
//         .body(buffer.into_inner())
// }

#[actix_web::main]
async fn main() -> std::io::Result<()> {
let regular_path = std::env::var("FONT_REGULAR_PATH")
        .unwrap_or_else(|_| "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".to_string());
    let bold_path = std::env::var("FONT_BOLD_PATH")
        .unwrap_or_else(|_| "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc".to_string());

    let font_regular = FontArc::try_from_vec(std::fs::read(&regular_path).expect("Regular font not found"))
        .expect("Failed to parse regular font");
    let font_bold = FontArc::try_from_vec(std::fs::read(&bold_path).expect("Bold font not found"))
        .expect("Failed to parse bold font");

    // ToDo: 画像を選択できるようにする
    let base_url = "https://tracc.jp/og/sub-base.png";
    let image_data = reqwest::blocking::get(base_url)
        .expect("Failed to download base image")
        .bytes()
        .expect("Failed to get image bytes");
    
    let base_img = image::load_from_memory(&image_data)
        .expect("Failed to decode base image")
        .to_rgba8();

    let font_state = web::Data::new(FontState {
        regular: font_regular,
        bold: font_bold,
        base_image: base_img,
    });

    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let port: u16 = port.parse().expect("PORT must be a number");

    HttpServer::new(move || {
        let cors = Cors::default()
            .allow_any_origin() // 利用環境に応じて設定する必要あり
            .allow_any_method()
            .allow_any_header();

        App::new()
            .wrap(cors)
            .app_data(font_state.clone())
            .route("/", web::get().to(|| async { HttpResponse::Ok().body("OGP Generator is Running!") }))
            .service(compress_handler)
            .service(compress_webp_handler)
            // .service(ogp_handler)
            .service(ogp_get_handler)
    })
    .bind(("0.0.0.0", port))? // Cloud Run 対応
    .run()
    .await
}
