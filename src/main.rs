use actix_cors::Cors;
use actix_multipart::Multipart;
use actix_web::{post, App, HttpServer, HttpResponse, Responder, web};
use futures_util::TryStreamExt;
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba};
use std::io::Cursor;

use ab_glyph::{FontArc, PxScale};
use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};
use serde::Deserialize;

const JPEG_QUALITY: u8 = 85;

struct FontState {
    regular: FontArc,
    bold: FontArc,
    base_image: ImageBuffer<Rgba<u8>, Vec<u8>>,
}

fn decode_raw(bytes: &[u8]) -> Option<DynamicImage> {
    // NEF は TIFF コンテナ (II*\0 or MM\0*)
    if !(bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*")) {
        return None;
    }
    let raw = rawloader::decode(&mut Cursor::new(bytes)).ok()?;
    let mut pipeline =
        imagepipe::Pipeline::new_from_source(imagepipe::ImageSource::Raw(raw)).ok()?;
    let decoded = pipeline.output_8bit(None).ok()?;
    let buf = image::RgbImage::from_raw(
        decoded.width as u32,
        decoded.height as u32,
        decoded.data,
    )?;
    Some(DynamicImage::ImageRgb8(buf))
}

fn is_heic(bytes: &[u8]) -> bool {
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    matches!(
        &bytes[8..12],
        b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"mif1" | b"msf1" | b"heif"
    )
}

fn decode_heic(bytes: &[u8]) -> Option<DynamicImage> {
    let lib = LibHeif::new();
    let ctx = HeifContext::read_from_bytes(bytes).ok()?;
    let handle = ctx.primary_image_handle().ok()?;
    let has_alpha = handle.has_alpha_channel();
    let chroma = if has_alpha { RgbChroma::Rgba } else { RgbChroma::Rgb };
    let img = lib.decode(&handle, ColorSpace::Rgb(chroma), None).ok()?;

    let plane = img.planes().interleaved?;
    let w = plane.width as usize;
    let h = plane.height as usize;
    let bpp = if has_alpha { 4 } else { 3 };
    let stride = plane.stride;

    let mut data = Vec::with_capacity(w * h * bpp);
    for y in 0..h {
        let start = y * stride;
        data.extend_from_slice(plane.data.get(start..start + w * bpp)?);
    }

    if has_alpha {
        image::RgbaImage::from_raw(w as u32, h as u32, data).map(DynamicImage::ImageRgba8)
    } else {
        image::RgbImage::from_raw(w as u32, h as u32, data).map(DynamicImage::ImageRgb8)
    }
}

fn strip_exif_item_type(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let limit = out.len().min(1 << 20); // meta ボックスは先頭付近にある
    let mut i = 4;
    while i + 8 <= limit {
        if &out[i..i + 4] == b"infe" {
            let box_start = i - 4;
            let size = u32::from_be_bytes([
                out[box_start], out[box_start + 1], out[box_start + 2], out[box_start + 3],
            ]) as usize;
            let version = out[i + 4];
            if version >= 2 && box_start + size <= out.len() {
                let id_len = if version == 2 { 2 } else { 4 };
                let type_pos = i + 8 + id_len + 2;
                if type_pos + 4 <= box_start + size && &out[type_pos..type_pos + 4] == b"Exif" {
                    out[type_pos..type_pos + 4].copy_from_slice(b"Exi_");
                }
            }
            i += 4;
        } else {
            i += 1;
        }
    }
    out
}

fn decode_image(bytes: &[u8]) -> Result<DynamicImage, image::ImageError> {
    if let Some(img) = decode_raw(bytes) {
        return Ok(img);
    }
    if is_heic(bytes) {
        if let Some(img) = decode_heic(&strip_exif_item_type(bytes)) {
            return Ok(img);
        }
    }
    image::load_from_memory(bytes)
}

fn extract_exif_jpeg(image_bytes: &[u8]) -> Option<Vec<u8>> {
    // JPEG の場合 (SOIマーカー 0xFF 0xD8)
    if image_bytes.len() < 4 || image_bytes[0] != 0xFF || image_bytes[1] != 0xD8 {
        return None;
    }
    let mut idx = 2;
    while idx + 4 < image_bytes.len() {
        if image_bytes[idx] != 0xFF {
            break;
        }
        let marker = image_bytes[idx + 1];
            if marker == 0xDA { // SOS
            break;
        }
        let length = u16::from_be_bytes([image_bytes[idx + 2], image_bytes[idx + 3]]) as usize;
            if marker == 0xE1 { // APP1 (Exif)
            let payload_start = idx + 4;
            let payload_end = idx + 2 + length;
            if payload_end <= image_bytes.len()
                && payload_end - payload_start >= 6
                && &image_bytes[payload_start..payload_start + 6] == b"Exif\0\0"
            {
                return Some(image_bytes[idx..payload_end].to_vec());
            }
        }
        idx += 2 + length;
    }
    None
}

fn reset_orientation(tiff: &mut [u8]) {
    fn find_pos(tiff: &[u8]) -> Option<(usize, bool)> {
        let le = match tiff.get(0..2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let rd16 = |o: usize| -> Option<u16> {
            let s = tiff.get(o..o + 2)?;
            Some(if le {
                u16::from_le_bytes([s[0], s[1]])
            } else {
                u16::from_be_bytes([s[0], s[1]])
            })
        };
        let rd32 = |o: usize| -> Option<u32> {
            let s = tiff.get(o..o + 4)?;
            Some(if le {
                u32::from_le_bytes([s[0], s[1], s[2], s[3]])
            } else {
                u32::from_be_bytes([s[0], s[1], s[2], s[3]])
            })
        };
        let ifd0 = rd32(4)? as usize;
        let count = rd16(ifd0)? as usize;
        for i in 0..count {
            let entry = ifd0 + 2 + i * 12;
            if rd16(entry)? == 0x0112 {
                return Some((entry + 8, le));
            }
        }
        None
    }

    if let Some((pos, le)) = find_pos(tiff) {
        if pos + 2 <= tiff.len() {
            let v: [u8; 2] = if le { [1, 0] } else { [0, 1] };
            tiff[pos..pos + 2].copy_from_slice(&v);
        }
    }
}

fn extract_exif_heic(bytes: &[u8]) -> Option<Vec<u8>> {
    let ctx = HeifContext::read_from_bytes(bytes).ok()?;
    let handle = ctx.primary_image_handle().ok()?;
    let mut ids = [0u32; 8];
    let n = handle.metadata_block_ids(&mut ids, b"Exif");
    if n == 0 {
        return None;
    }
    let id = ids[0];
    let data = handle.metadata(id).ok()?;
    if data.len() < 4 {
        return None;
    }

    let offset = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let mut tiff = data.get(4 + offset..)?.to_vec();
    reset_orientation(&mut tiff);

    let len = tiff.len() + 2 + 6;
    if len > u16::MAX as usize {
        return None;
    }
    let mut seg = vec![0xFF, 0xE1];
    seg.extend_from_slice(&(len as u16).to_be_bytes());
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(&tiff);
    Some(seg)
}

fn extract_exif(bytes: &[u8]) -> Option<Vec<u8>> {
    if is_heic(bytes) {
        None
    } else {
        extract_exif_jpeg(bytes)
    }
}

fn inject_exif(jpeg_bytes: &[u8], exif_segment: &[u8]) -> Vec<u8> {
    if jpeg_bytes.len() < 2 || jpeg_bytes[0] != 0xFF || jpeg_bytes[1] != 0xD8 {
        return jpeg_bytes.to_vec();
    }

    let mut result = Vec::with_capacity(jpeg_bytes.len() + exif_segment.len());
    result.extend_from_slice(&[0xFF, 0xD8]); // SOI
    result.extend_from_slice(exif_segment); // Exif (APP1) を挿入
    result.extend_from_slice(&jpeg_bytes[2..]); // 残りの画像データ
    result
}

fn encode_jpeg(img: &DynamicImage, quality: u8) -> Result<Vec<u8>, image::ImageError> {
    let rgb = img.to_rgb8();
    let mut buf = Vec::new();
    JpegEncoder::new_with_quality(&mut buf, quality).encode_image(&rgb)?;
    Ok(buf)
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

async fn read_multipart(payload: &mut Multipart) -> Vec<u8> {
    let mut image_bytes = Vec::new();

    // 1. ファイルをメモリに読み込む
    while let Ok(Some(mut field)) = payload.try_next().await {
        while let Ok(Some(chunk)) = field.try_next().await {
            image_bytes.extend_from_slice(&chunk);
        }
    }
    image_bytes
}

#[post("/compress")]
async fn compress_handler(mut payload: Multipart) -> impl Responder {
    let image_bytes = read_multipart(&mut payload).await;

    if image_bytes.is_empty() {
        return HttpResponse::BadRequest().body("No data");
    }

    // 元画像から Exif（APP1 セグメント）をあらかじめ抽出しておく
    let exif_data = extract_exif(&image_bytes);

    let mut img = match decode_image(&image_bytes) {
        Ok(img) => img,
        Err(_) => return HttpResponse::BadRequest().body("Decode error"),
    };

    // 3. 透過 PNG 対策：アルファチャンネルがあれば白背景と合成
    if img.color().has_alpha() {
        let rgba = img.to_rgba8();
        // 背景を白 (255, 255, 255, 255) で作成
        let mut background = ImageBuffer::from_pixel(
            img.width(),
            img.height(),
            Rgba([255u8, 255, 255, 255]),
        );

        // アルファブレンディング (透過を考慮して重ねる)
        image::imageops::overlay(&mut background, &rgba, 0, 0);
        img = DynamicImage::ImageRgba8(background);
    }

    // 4. リサイズ (短辺 1024px)
    // let scaled = img.thumbnail(1024, 1024);
    // 意図的にリサイズしない
    // let img = img.thumbnail(1024, 1024);

    // 5. JPG エンコード
    let Ok(mut final_bytes) = encode_jpeg(&img, JPEG_QUALITY) else {
        return HttpResponse::InternalServerError().body("Encode error");
    };

    // 抽出できた Exif があれば、再エンコード後の JPEG に再度埋め込む
    if let Some(exif) = exif_data {
        final_bytes = inject_exif(&final_bytes, &exif);
    }

    HttpResponse::Ok()
        .content_type("image/jpeg")
        .body(final_bytes)
}

#[post("/compress_webp")]
async fn compress_webp_handler(mut payload: Multipart) -> impl Responder {
    let image_bytes = read_multipart(&mut payload).await;

    if image_bytes.is_empty() {
        return HttpResponse::BadRequest().body("No data");
    }

    // 1. 画像デコード (PNG or JPG or TIFF などを自動で判別)
    let Ok(img) = decode_image(&image_bytes) else {
        return HttpResponse::BadRequest().body("Decode error");
    };

    // 2. リサイズ処理 (共通)
    // let scaled = img.thumbnail(1024, 1024);
    // 意図的にリサイズしない
    let scaled = img;

    // 3. WebP エンコード
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
        draw_wrapped_text(
            &mut canvas,
            &state.bold,
            &title,
            60,
            210,
            PxScale::from(64.0),
            Rgba([26, 26, 26, 255]),
            1080,
            76,
        );
    }
    if !subtitle.is_empty() {
        draw_wrapped_text(
            &mut canvas,
            &state.regular,
            &subtitle,
            60,
            355,
            PxScale::from(36.0),
            Rgba([117, 117, 117, 255]),
            1080,
            44,
        );
    }

    match encode_jpeg(&DynamicImage::ImageRgba8(canvas), JPEG_QUALITY) {
        Ok(bytes) => HttpResponse::Ok().content_type("image/jpeg").body(bytes),
        Err(_) => HttpResponse::InternalServerError().body("Encode error"),
    }
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
    let base_url = "https://admin.tracc.jp/og/sub-base.png";
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
    .bind_auto_h2c(("0.0.0.0", port))? // Cloud Run 対応
    .run()
    .await
}
