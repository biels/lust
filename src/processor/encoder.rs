use crate::config::{AvifFormatConfig, ImageFormats, ImageKind};
use bytes::Bytes;
use image::{DynamicImage, ImageFormat};
use ravif::{Config as AvifConfig, Encoder as AvifEncoder, RGBA8};
use std::io::Cursor;
use std::sync::Arc;

pub struct EncodedImage {
    pub kind: ImageKind,
    pub buff: Bytes,
    pub sizing_id: u32,
}

pub fn encode_following_config(
    cfg: ImageFormats,
    img: DynamicImage,
    sizing_id: u32,
) -> anyhow::Result<Vec<EncodedImage>> {
    let original_image = Arc::new(img);

    let webp_config = webp::config(
        cfg.webp_config.quality.is_none(),
        cfg.webp_config.quality.unwrap_or(50f32),
        cfg.webp_config.method.unwrap_or(4) as i32,
        cfg.webp_config.threading,
    );

    let (tx, rx) = crossbeam::channel::bounded(4);

    // let avif_config_from_cfg = cfg.avif_config; // This is now available

    for variant in ImageKind::variants() {
        if cfg.is_enabled(*variant) {
            let tx_local = tx.clone();
            let local = original_image.clone();
            // Pass the actual avif_config from the bucket's format settings
            let avif_config_to_pass = if *variant == ImageKind::Avif { Some(cfg.avif_config) } else { None };
            rayon::spawn(move || {
                let result = encode_to(webp_config, avif_config_to_pass, &local, *variant);
                tx_local
                    .send(result.map(|v| EncodedImage {
                        kind: *variant,
                        buff: v,
                        sizing_id,
                    }))
                    .expect("Failed to respond to encoding request. Sender already closed.");
            });
        }
    }

    // Needed to prevent deadlock.
    drop(tx);

    let mut processed = vec![];
    while let Ok(encoded) = rx.recv() {
        processed.push(encoded);
    }

    let finished = processed
        .into_iter()
        .collect::<Result<Vec<EncodedImage>, _>>()?;

    Ok(finished)
}

pub fn encode_once(
    webp_cfg: webp::WebPConfig,
    avif_cfg_opt: Option<AvifFormatConfig>, // Added
    to: ImageKind,
    img: DynamicImage,
    sizing_id: u32,
) -> anyhow::Result<EncodedImage> {
    let (tx, rx) = crossbeam::channel::bounded(4);

    rayon::spawn(move || {
        let result = encode_to(webp_cfg, avif_cfg_opt, &img, to); // Modified call
        tx.send(result.map(|v| EncodedImage {
            kind: to,
            buff: v,
            sizing_id,
        }))
        .expect("Failed to respond to encoding request. Sender already closed.");
    });

    rx.recv()?
}

#[inline]
pub fn encode_to(
    webp_cfg: webp::WebPConfig,
    avif_cfg_opt: Option<AvifFormatConfig>, // Added
    img: &DynamicImage,
    image_kind: ImageKind, // Changed from format: ImageFormat
) -> anyhow::Result<Bytes> {
    if image_kind == ImageKind::Webp {
        let webp_image = webp::Encoder::from_image(webp_cfg, img);
        let encoded = webp_image.encode();
        Ok(Bytes::from(encoded?.to_vec()))
    } else if image_kind == ImageKind::Avif {
        let rgba_img = img.to_rgba8();
        let width = rgba_img.width();
        let height = rgba_img.height();

        // Convert image::Rgba<u8> pixels to ravif::RGBA8 pixels
        // Rgba<u8>.0 is [u8; 4], so p.0 gives [u8; 4]
        // RGBA8::new takes r, g, b, a as u8
        let pixels: Vec<RGBA8> = rgba_img
            .pixels()
            .map(|p| RGBA8::new(p[0], p[1], p[2], p[3]))
            .collect();

        let ravif_config = avif_cfg_opt.map_or_else(
            AvifConfig::default, // Use ravif's default if no specific config is provided
            |c| AvifConfig {
                quality: c.quality.unwrap_or(80.0), 
                alpha_quality: c.alpha_quality.unwrap_or(100.0), // 100 for lossless alpha is common
                speed: c.speed.unwrap_or(6), // ravif speed: 0 (best quality) to 10 (fastest)
                ..Default::default() 
            },
        );

        let avif_data = AvifEncoder::new(ravif_config).encode_rgba(width, height, &pixels)?;
        Ok(Bytes::from(avif_data))
    } else {
        let mut buff = Cursor::new(Vec::new());
        // Convert our ImageKind to the image crate's ImageFormat for other formats
        let format_for_image_crate: ImageFormat = image_kind.into();

        // Convert the image to RGB if it's RGBA and we're encoding to JPEG
        let img_to_encode = if format_for_image_crate == ImageFormat::Jpeg && img.color() == image::ColorType::Rgba8 {
            DynamicImage::ImageRgb8(img.to_rgb8())
        } else {
            img.clone()
        };

        img_to_encode.write_to(&mut buff, format_for_image_crate)?;
        Ok(Bytes::from(buff.into_inner()))
    }
}
