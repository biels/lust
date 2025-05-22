use image::load_from_memory_with_format;
use poem::http::StatusCode;
use poem::test::{TestClient, TestResponse};
use poem::web::headers;
use poem::Route;
use poem_openapi::OpenApiService;
use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::{cache, config, controller, BucketController, StorageBackend};

const JIT_CONFIG: &str = include_str!("../tests/configs/jit-mode.yaml");
const AOT_CONFIG: &str = include_str!("../tests/configs/aot-mode.yaml");
const REALTIME_CONFIG: &str = include_str!("../tests/configs/realtime-mode.yaml");

const CACHE_TEST_JIT_CONFIG_STRING: &str = r#"
global_cache: {}
buckets:
  cache-test-jit:
    mode: jit
    formats:
      input: [jpeg, png]
      output: [jpeg, png, webp]
      default_output: jpeg
      original_image_store_format: jpeg
    presets:
      small:
        width: 50
        height: 50
        format: [png]
      large:
        width: 100
        height: 100
        format: [png]
    cache:
      max_images: 2
    backend:
      type: filesystem
      directory: "test_data_cache_jit"
"#;

const CACHE_TEST_AOT_CONFIG_STRING: &str = r#"
global_cache: {}
buckets:
  cache-test-aot:
    mode: aot
    formats:
      input: [jpeg, png]
      output: [webp]
      default_output: webp
    presets:
      thumbnail:
        width: 120
        height: 120
        format: [webp]
    cache:
      max_images: 2
    backend:
      type: filesystem
      directory: "test_data_cache_aot"
"#;

const TEST_IMAGE: &[u8] = include_bytes!("../examples/example.jpeg");

async fn setup_environment(cfg: &str) -> anyhow::Result<TestClient<Route>> {
    config::init_test(cfg)?;

    let global_limiter = config::config()
        .max_concurrency
        .map(Semaphore::new)
        .map(Arc::new);

    let storage: Arc<dyn StorageBackend> = config::config().backend.connect().await?;

    let buckets = config::config()
        .buckets
        .iter()
        .map(|(bucket, cfg)| {
            let bucket_id = crate::utils::crc_hash(bucket);
            let pipeline = cfg.mode.build_pipeline(cfg);
            let cache = cfg.cache.map(cache::new_cache).transpose()?.flatten();

            let controller = BucketController::new(
                bucket_id,
                cache,
                global_limiter.clone(),
                cfg.clone(),
                pipeline,
                storage.clone(),
            );
            Ok::<_, anyhow::Error>((bucket_id, controller))
        })
        .collect::<Result<hashbrown::HashMap<_, _>, anyhow::Error>>()?;

    controller::init_buckets(buckets);

    let app = OpenApiService::new(
        crate::routes::LustApi,
        "Lust API",
        env!("CARGO_PKG_VERSION"),
    );

    let app = Route::new().nest("/v1", app);
    Ok(TestClient::new(app))
}

async fn validate_image_content(
    res: TestResponse,
    expected_format: image::ImageFormat,
) -> anyhow::Result<()> {
    let body = res.0.into_body().into_bytes().await?;

    load_from_memory_with_format(&body, expected_format)
        .expect("Invalid image returned for expected format");

    Ok(())
}

async fn get_image_dimensions(
    res: TestResponse,
    expected_format: image::ImageFormat,
) -> anyhow::Result<(u32, u32)> {
    let body = res.0.into_body().into_bytes().await?;
    let img = image::load_from_memory_with_format(&body, expected_format)?;
    Ok(img.dimensions())
}

#[tokio::test]
async fn test_jit_cache_hit_and_correctness_custom_size() -> anyhow::Result<()> {
    let app = setup_environment(CACHE_TEST_JIT_CONFIG_STRING).await?;

    let res = app
        .post("/v1/cache-test-jit")
        .body(TEST_IMAGE)
        .content_type("image/jpeg")
        .typed_header(headers::ContentLength(TEST_IMAGE.len() as u64))
        .send()
        .await;
    res.assert_status(StatusCode::OK);
    let info = res.json().await;
    let file_id = info.value().object().get("image_id").string();

    // Request 1
    let res1 = app
        .get(format!("/v1/cache-test-jit/{}", file_id))
        .query("format", &"png")
        .query("width", &70)
        .query("height", &70)
        .send()
        .await;
    res1.assert_status(StatusCode::OK);
    let bytes1 = res1.0.into_body().into_bytes().await?;
    let (w1, h1) =
        image::load_from_memory_with_format(&bytes1, image::ImageFormat::Png)?.dimensions();
    assert_eq!((w1, h1), (70, 70));

    // Request 2 (should hit cache)
    let res2 = app
        .get(format!("/v1/cache-test-jit/{}", file_id))
        .query("format", &"png")
        .query("width", &70)
        .query("height", &70)
        .send()
        .await;
    res2.assert_status(StatusCode::OK);
    let bytes2 = res2.0.into_body().into_bytes().await?;
    let (w2, h2) =
        image::load_from_memory_with_format(&bytes2, image::ImageFormat::Png)?.dimensions();
    assert_eq!((w2, h2), (70, 70));

    assert_eq!(bytes1, bytes2, "Bytes from initial request and cached request should be identical");

    Ok(())
}

#[tokio::test]
async fn test_jit_custom_dimensions_cached_separately() -> anyhow::Result<()> {
    let app = setup_environment(CACHE_TEST_JIT_CONFIG_STRING).await?;

    let res = app
        .post("/v1/cache-test-jit")
        .body(TEST_IMAGE)
        .content_type("image/jpeg")
        .typed_header(headers::ContentLength(TEST_IMAGE.len() as u64))
        .send()
        .await;
    res.assert_status(StatusCode::OK);
    let info = res.json().await;
    let file_id = info.value().object().get("image_id").string();

    // Request 1: CustomSmall (60x60)
    let res1 = app
        .get(format!("/v1/cache-test-jit/{}", file_id))
        .query("format", &"png")
        .query("width", &60)
        .query("height", &60)
        .send()
        .await;
    res1.assert_status(StatusCode::OK);
    let r1_custom_small_bytes = res1.0.into_body().into_bytes().await?;
    let (w1, h1) = image::load_from_memory_with_format(
        &r1_custom_small_bytes,
        image::ImageFormat::Png,
    )?
    .dimensions();
    assert_eq!((w1, h1), (60, 60));

    // Request 2: CustomLarge (90x90)
    let res2 = app
        .get(format!("/v1/cache-test-jit/{}", file_id))
        .query("format", &"png")
        .query("width", &90)
        .query("height", &90)
        .send()
        .await;
    res2.assert_status(StatusCode::OK);
    let r2_custom_large_bytes = res2.0.into_body().into_bytes().await?;
    let (w2, h2) = image::load_from_memory_with_format(
        &r2_custom_large_bytes,
        image::ImageFormat::Png,
    )?
    .dimensions();
    assert_eq!((w2, h2), (90, 90));

    assert_ne!(
        r1_custom_small_bytes, r2_custom_large_bytes,
        "CustomSmall and CustomLarge should be different images"
    );

    // Request 3: CustomSmall (60x60) - should be a cache hit
    let res3 = app
        .get(format!("/v1/cache-test-jit/{}", file_id))
        .query("format", &"png")
        .query("width", &60)
        .query("height", &60)
        .send()
        .await;
    res3.assert_status(StatusCode::OK);
    let r3_custom_small_bytes = res3.0.into_body().into_bytes().await?;

    assert_eq!(
        r1_custom_small_bytes, r3_custom_small_bytes,
        "R1_CustomSmall and R3_CustomSmall should be identical"
    );

    Ok(())
}

#[tokio::test]
async fn test_jit_presets_cached_separately() -> anyhow::Result<()> {
    let app = setup_environment(CACHE_TEST_JIT_CONFIG_STRING).await?;

    let res = app
        .post("/v1/cache-test-jit")
        .body(TEST_IMAGE)
        .content_type("image/jpeg")
        .typed_header(headers::ContentLength(TEST_IMAGE.len() as u64))
        .send()
        .await;
    res.assert_status(StatusCode::OK);
    let info = res.json().await;
    let file_id = info.value().object().get("image_id").string();

    // Request 1: PresetSmall
    let res1 = app
        .get(format!("/v1/cache-test-jit/{}", file_id))
        .query("format", &"png")
        .query("preset", &"small")
        .send()
        .await;
    res1.assert_status(StatusCode::OK);
    let r1_preset_small_bytes = res1.0.into_body().into_bytes().await?;
    let (w1, h1) = image::load_from_memory_with_format(
        &r1_preset_small_bytes,
        image::ImageFormat::Png,
    )?
    .dimensions();
    assert_eq!((w1, h1), (50, 50));

    // Request 2: PresetLarge
    let res2 = app
        .get(format!("/v1/cache-test-jit/{}", file_id))
        .query("format", &"png")
        .query("preset", &"large")
        .send()
        .await;
    res2.assert_status(StatusCode::OK);
    let r2_preset_large_bytes = res2.0.into_body().into_bytes().await?;
    let (w2, h2) = image::load_from_memory_with_format(
        &r2_preset_large_bytes,
        image::ImageFormat::Png,
    )?
    .dimensions();
    assert_eq!((w2, h2), (100, 100));

    assert_ne!(
        r1_preset_small_bytes, r2_preset_large_bytes,
        "PresetSmall and PresetLarge should be different images"
    );

    // Request 3: PresetSmall - should be a cache hit
    let res3 = app
        .get(format!("/v1/cache-test-jit/{}", file_id))
        .query("format", &"png")
        .query("preset", &"small")
        .send()
        .await;
    res3.assert_status(StatusCode::OK);
    let r3_preset_small_bytes = res3.0.into_body().into_bytes().await?;

    assert_eq!(
        r1_preset_small_bytes, r3_preset_small_bytes,
        "R1_PresetSmall and R3_PresetSmall should be identical"
    );

    Ok(())
}

#[tokio::test]
async fn test_aot_image_is_cached_on_fetch() -> anyhow::Result<()> {
    let app = setup_environment(CACHE_TEST_AOT_CONFIG_STRING).await?;

    // Upload image (AOT processing happens here)
    let res_upload = app
        .post("/v1/cache-test-aot")
        .body(TEST_IMAGE)
        .content_type("image/jpeg") // Specify input format
        .query("format", &"jpeg") // Explicitly state original format for AOT
        .typed_header(headers::ContentLength(TEST_IMAGE.len() as u64))
        .send()
        .await;
    res_upload.assert_status(StatusCode::OK);
    let info = res_upload.json().await;
    let file_id = info.value().object().get("image_id").string();

    // Request 1: Fetch the image (default format for AOT is webp as per config)
    let res1 = app
        .get(format!("/v1/cache-test-aot/{}", file_id))
        .send()
        .await;
    res1.assert_status(StatusCode::OK);
    res1.assert_content_type(&"image/webp".to_string());
    let bytes1 = res1.0.into_body().into_bytes().await?;
    validate_image_content(TestResponse(bytes1.clone().into()), image::ImageFormat::WebP).await?;


    // Request 2: Fetch the same image again (should be served from processed cache)
    let res2 = app
        .get(format!("/v1/cache-test-aot/{}", file_id))
        .send()
        .await;
    res2.assert_status(StatusCode::OK);
    res2.assert_content_type(&"image/webp".to_string());
    let bytes2 = res2.0.into_body().into_bytes().await?;
    validate_image_content(TestResponse(bytes2.clone().into()), image::ImageFormat::WebP).await?;

    assert_eq!(bytes1, bytes2, "Bytes from initial fetch and cached fetch should be identical for AOT");

    Ok(())
}


#[tokio::test]
async fn test_basic_aot_upload_retrieval_without_guessing() -> anyhow::Result<()> {
    let app = setup_environment(AOT_CONFIG).await?;

    let res = app
        .post("/v1/user-profiles")
        .body(TEST_IMAGE)
        .content_type("application/octet-stream".to_string())
        .typed_header(headers::ContentLength(TEST_IMAGE.len() as u64))
        .query("format".to_string(), &"jpeg".to_string())
        .send()
        .await;

    res.assert_status(StatusCode::OK);
    let info = res.json().await;

    let file_id = info.value().object().get("image_id").string();

    let res = app
        .get(format!("/v1/user-profiles/{}", file_id))
        .send()
        .await;

    res.assert_status(StatusCode::OK);
    res.assert_content_type(&"image/webp".to_string());

    validate_image_content(res, image::ImageFormat::WebP).await?;

    Ok(())
}

#[tokio::test]
async fn test_basic_aot_upload_retrieval_with_guessing() -> anyhow::Result<()> {
    let app = setup_environment(AOT_CONFIG).await?;

    let res = app
        .post("/v1/user-profiles")
        .body(TEST_IMAGE)
        .content_type("application/octet-stream".to_string())
        .typed_header(headers::ContentLength(TEST_IMAGE.len() as u64))
        .query("format".to_string(), &"jpeg".to_string())
        .send()
        .await;

    res.assert_status(StatusCode::OK);
    let info = res.json().await;

    let file_id = info.value().object().get("image_id").string();

    let res = app
        .get(format!("/v1/user-profiles/{}", file_id))
        .send()
        .await;

    res.assert_status(StatusCode::OK);
    res.assert_content_type(&"image/webp".to_string());

    validate_image_content(res, image::ImageFormat::WebP).await?;

    Ok(())
}

#[tokio::test]
async fn test_basic_jit_upload_retrieval() -> anyhow::Result<()> {
    let app = setup_environment(JIT_CONFIG).await?;

    let res = app
        .post("/v1/user-profiles")
        .body(TEST_IMAGE)
        .content_type("application/octet-stream".to_string())
        .typed_header(headers::ContentLength(TEST_IMAGE.len() as u64))
        .query("format".to_string(), &"jpeg".to_string())
        .send()
        .await;

    res.assert_status(StatusCode::OK);
    let info = res.json().await;

    let file_id = info.value().object().get("image_id").string();

    let res = app
        .get(format!("/v1/user-profiles/{}", file_id))
        .send()
        .await;

    res.assert_status(StatusCode::OK);
    res.assert_content_type(&"image/jpeg".to_string());

    validate_image_content(res, image::ImageFormat::Jpeg).await?;

    Ok(())
}

#[tokio::test]
async fn test_jit_upload_custom_format_retrieval() -> anyhow::Result<()> {
    let app = setup_environment(JIT_CONFIG).await?;

    let res = app
        .post("/v1/user-profiles")
        .body(TEST_IMAGE)
        .content_type("application/octet-stream".to_string())
        .typed_header(headers::ContentLength(TEST_IMAGE.len() as u64))
        .query("format".to_string(), &"jpeg".to_string())
        .send()
        .await;

    res.assert_status(StatusCode::OK);
    let info = res.json().await;

    let file_id = info.value().object().get("image_id").string();

    let res = app
        .get(format!("/v1/user-profiles/{}", file_id))
        .query("format", &"png".to_string())
        .send()
        .await;

    res.assert_status(StatusCode::OK);
    res.assert_content_type(&"image/png".to_string());

    validate_image_content(res, image::ImageFormat::Png).await?;

    Ok(())
}

#[tokio::test]
async fn test_basic_realtime_upload_retrieval() -> anyhow::Result<()> {
    let app = setup_environment(REALTIME_CONFIG).await?;

    let res = app
        .post("/v1/user-profiles")
        .body(TEST_IMAGE)
        .content_type("application/octet-stream".to_string())
        .typed_header(headers::ContentLength(TEST_IMAGE.len() as u64))
        .query("format".to_string(), &"jpeg".to_string())
        .send()
        .await;

    res.assert_status(StatusCode::OK);
    let info = res.json().await;

    let file_id = info.value().object().get("image_id").string();

    let res = app
        .get(format!("/v1/user-profiles/{}", file_id))
        .send()
        .await;

    res.assert_status(StatusCode::OK);
    res.assert_content_type(&"image/png".to_string());

    validate_image_content(res, image::ImageFormat::Png).await?;

    Ok(())
}

#[tokio::test]
async fn test_realtime_resizing() -> anyhow::Result<()> {
    let app = setup_environment(REALTIME_CONFIG).await?;

    let res = app
        .post("/v1/user-profiles")
        .body(TEST_IMAGE)
        .content_type("application/octet-stream".to_string())
        .typed_header(headers::ContentLength(TEST_IMAGE.len() as u64))
        .query("format".to_string(), &"jpeg".to_string())
        .send()
        .await;

    res.assert_status(StatusCode::OK);
    let info = res.json().await;

    let file_id = info.value().object().get("image_id").string();

    let res = app
        .get(format!("/v1/user-profiles/{}", file_id))
        .query("width".to_string(), &"500".to_string())
        .query("height".to_string(), &"500".to_string())
        .send()
        .await;

    res.assert_status(StatusCode::OK);
    res.assert_content_type(&"image/png".to_string());

    validate_image_content(res, image::ImageFormat::Png).await?;

    Ok(())
}

#[tokio::test]
async fn test_realtime_resizing_expect_err() -> anyhow::Result<()> {
    let app = setup_environment(REALTIME_CONFIG).await?;

    let res = app
        .post("/v1/user-profiles")
        .body(TEST_IMAGE)
        .content_type("application/octet-stream".to_string())
        .typed_header(headers::ContentLength(TEST_IMAGE.len() as u64))
        .send()
        .await;

    res.assert_status(StatusCode::OK);
    let info = res.json().await;

    let file_id = info.value().object().get("image_id").string();

    let res = app
        .get(format!("/v1/user-profiles/{}", file_id))
        .query("width".to_string(), &"500".to_string())
        .send()
        .await;

    res.assert_status(StatusCode::BAD_REQUEST);

    Ok(())
}
