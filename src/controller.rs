use crate::cache::{global_cache, Cache};
use bytes::Bytes;
use once_cell::sync::OnceCell;
use poem_openapi::Object;
use std::hash::Hash;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Semaphore, SemaphorePermit};

use crate::config::{BucketConfig, ImageKind};
use crate::pipelines::{PipelineController, ProcessingMode, StoreEntry};
use crate::storage::template::StorageBackend;

static BUCKETS: OnceCell<hashbrown::HashMap<u32, BucketController>> = OnceCell::new();

pub fn init_buckets(buckets: hashbrown::HashMap<u32, BucketController>) {
    let _ = BUCKETS.set(buckets);
}

pub fn get_bucket_by_id(bucket_id: u32) -> Option<&'static BucketController> {
    BUCKETS.get_or_init(hashbrown::HashMap::new).get(&bucket_id)
}

pub fn get_bucket_by_name(bucket: impl Hash) -> Option<&'static BucketController> {
    let bucket_id = crate::utils::crc_hash(bucket);
    get_bucket_by_id(bucket_id)
}

async fn get_optional_permit<'a>(
    global: &'a Option<Arc<Semaphore>>,
    local: &'a Option<Semaphore>,
) -> anyhow::Result<Option<SemaphorePermit<'a>>> {
    if let Some(limiter) = global {
        return Ok(Some(limiter.acquire().await?));
    }

    if let Some(limiter) = local {
        return Ok(Some(limiter.acquire().await?));
    }

    Ok(None)
}

#[derive(Object, Debug)]
pub struct ImageUploadInfo {
    /// The computed image sizing id.
    ///
    /// This is useful for tracking files outside of lust as this is
    /// generally used for filtering within the storage systems.
    sizing_id: u32,
}

#[derive(Object, Debug)]
pub struct UploadInfo {
    /// The generated ID for the file.
    ///
    /// This can be used to access the file for the given bucket.
    image_id: String,

    /// The time spent processing the image in seconds.
    processing_time: f32,

    /// The time spent uploading the image to the persistent store.
    io_time: f32,

    /// The crc32 checksum of the uploaded image.
    checksum: u32,

    /// The information that is specific to the image.
    images: Vec<ImageUploadInfo>,

    /// The id of the bucket the image was stored in.
    ///
    /// This is useful for tracking files outside of lust as this is
    /// generally used for filtering within the storage systems.
    bucket_id: u32,
}

pub struct BucketController {
    bucket_id: u32,
    cache: Option<Arc<Cache>>,
    global_limiter: Option<Arc<Semaphore>>,
    config: BucketConfig,
    pipeline: PipelineController,
    storage: Arc<dyn StorageBackend>,
    limiter: Option<Semaphore>,
}

impl BucketController {
    pub fn new(
        bucket_id: u32,
        cache: Option<Cache>,
        global_limiter: Option<Arc<Semaphore>>,
        config: BucketConfig,
        pipeline: PipelineController,
        storage: Arc<dyn StorageBackend>,
    ) -> Self {
        Self {
            bucket_id,
            cache: cache.map(Arc::new),
            global_limiter,
            limiter: config.max_concurrency.map(Semaphore::new),
            config,
            pipeline,
            storage,
        }
    }

    #[inline]
    pub fn cfg(&self) -> &BucketConfig {
        &self.config
    }

    pub async fn upload(&self, kind: ImageKind, data: Vec<u8>) -> anyhow::Result<UploadInfo> {
        debug!(
            "Uploading processed image with kind: {:?} and is {} bytes in size.",
            kind,
            data.len()
        );

        let _permit = get_optional_permit(&self.global_limiter, &self.limiter).await?;

        let processing_start = Instant::now();
        let checksum = crc32fast::hash(&data);
        let image_id = crate::utils::sha256_hash(&data);

        let pipeline = self.pipeline.clone();
        let result = tokio::task::spawn_blocking(move || pipeline.on_upload(kind, data)).await??;
        let processing_time = processing_start.elapsed();

        let io_start = Instant::now();
        let image_upload_info = self
            .concurrent_upload(image_id.to_string(), result.result.to_store)
            .await?;
        let io_time = io_start.elapsed();

        Ok(UploadInfo {
            checksum,
            image_id,
            bucket_id: self.bucket_id,
            images: image_upload_info,
            processing_time: processing_time.as_secs_f32(),
            io_time: io_time.as_secs_f32(),
        })
    }

    pub async fn fetch(
        &self,
        image_id: &str,
        desired_kind: ImageKind,
        size_preset: Option<String>,
        custom_sizing: Option<(u32, u32)>,
    ) -> anyhow::Result<Option<StoreEntry>> {
        debug!(
            "Fetching image with image_id: {}, desired_kind: {:?}, preset: {:?}, custom_sizing: {:?}.",
            image_id, desired_kind, &size_preset, &custom_sizing,
        );

        let _permit = get_optional_permit(&self.global_limiter, &self.limiter).await?;

        let effective_sizing_key_part = if let Some((w, h)) = custom_sizing {
            format!("custom_{}x{}", w, h)
        } else if let Some(preset_name) = size_preset.as_ref().or(self.config.default_serving_preset.as_ref()) {
            if preset_name == "original" {
                format!("original_{}", 0)
            } else {
                format!("preset_{}", crate::utils::crc_hash(preset_name.clone()))
            }
        } else {
            format!("original_{}", 0)
        };

        let processed_image_cache_key = format!(
            "{bucket}:{sizing_key_part}:{image}:{kind}",
            bucket = self.bucket_id,
            sizing_key_part = effective_sizing_key_part,
            image = image_id,
            kind = desired_kind.as_file_extension(),
        );

        let cache_backend = self
            .cache
            .as_ref()
            .map(|c| c.as_ref())
            .or_else(global_cache);

        if let Some(cache) = cache_backend {
            if let Some(cached_data) = cache.get(&processed_image_cache_key) {
                debug!(
                    "Serving image from processed cache with key: {}",
                    processed_image_cache_key
                );
                let sizing_id_for_entry = if let Some(preset_name) =
                    size_preset.as_ref().or(self.config.default_serving_preset.as_ref())
                {
                    if preset_name == "original" {
                        0
                    } else {
                        crate::utils::crc_hash(preset_name.clone())
                    }
                } else {
                    0
                };
                return Ok(Some(StoreEntry {
                    data: cached_data,
                    kind: desired_kind,
                    sizing_id: sizing_id_for_entry,
                }));
            }
        }

        let sizing_for_storage_and_pipeline = size_preset
            .map(Some)
            .unwrap_or_else(|| self.config.default_serving_preset.clone());

        let sizing_id_for_storage_and_pipeline = if let Some(sizing_preset) = &sizing_for_storage_and_pipeline {
            if sizing_preset == "original" {
                0
            } else {
                crate::utils::crc_hash(sizing_preset.clone())
            }
        } else {
            0
        };

        // In real time situations
        let fetch_kind_for_storage = if self.config.mode == ProcessingMode::Realtime {
            self.config.formats.original_image_store_format
        } else {
            desired_kind
        };

        debug!(
            "Processed image cache miss for key: {}. Fetching from storage.",
            processed_image_cache_key
        );

        let maybe_existing = self
            .caching_fetch(
                image_id,
                fetch_kind_for_storage,
                if self.config.mode == ProcessingMode::Realtime {
                    0
                } else {
                    sizing_id_for_storage_and_pipeline
                },
            )
            .await?;

        let (source_data, source_kind) = match maybe_existing {
            None => {
                if self.config.mode == ProcessingMode::Jit || self.config.mode == ProcessingMode::Realtime {
                    let base_kind = self.config.formats.original_image_store_format;
                    let value = self.caching_fetch(image_id, base_kind, 0).await?;

                    match value {
                        None => return Ok(None),
                        Some(original) => (original, base_kind),
                    }
                } else {
                    return Ok(None);
                }
            }
            Some(computed) => (computed, fetch_kind_for_storage),
        };

        if self.config.mode == ProcessingMode::Aot {
            if let Some(cache) = cache_backend {
                debug!(
                    "Caching processed image in AOT mode with key: {}",
                    processed_image_cache_key
                );
                cache.insert(processed_image_cache_key.clone(), source_data.clone());
            }
            return Ok(Some(StoreEntry {
                data: source_data,
                kind: source_kind,
                sizing_id: sizing_id_for_storage_and_pipeline,
            }));
        }

        let pipeline = self.pipeline.clone();
        let result = tokio::task::spawn_blocking(move || {
            pipeline.on_fetch(
                desired_kind,
                source_kind,
                source_data,
                sizing_id_for_storage_and_pipeline,
                custom_sizing,
            )
        })
        .await??;

        if let Some(ref store_entry) = result.result.response {
            if let Some(cache) = cache_backend {
                debug!(
                    "Caching processed image in JIT/Realtime mode with key: {}",
                    processed_image_cache_key
                );
                cache.insert(processed_image_cache_key.clone(), store_entry.data.clone());
            }
        }

        self.concurrent_upload(image_id.to_string(), result.result.to_store)
            .await?;

        Ok(result.result.response)
    }

    pub async fn delete(&self, image_id: &str) -> anyhow::Result<()> {
        debug!("Removing image {}", image_id);

        let _permit = get_optional_permit(&self.global_limiter, &self.limiter).await?;
        let purged_entities = self.storage.delete(self.bucket_id, image_id).await?;

        if let Some(ref cache) = self.cache {
            for (sizing_id, kind) in purged_entities {
                let cache_key = self.cache_key(sizing_id, image_id, kind);
                cache.invalidate(&cache_key);
            }
        }

        Ok(())
    }
}

impl BucketController {
    #[inline]
    fn cache_key(&self, sizing_id: u32, image_id: &str, kind: ImageKind) -> String {
        format!(
            "{bucket}:{sizing}:{image}:{kind}",
            bucket = self.bucket_id,
            sizing = sizing_id,
            image = image_id,
            kind = kind.as_file_extension(),
        )
    }

    async fn caching_fetch(
        &self,
        image_id: &str,
        fetch_kind: ImageKind,
        sizing_id: u32,
    ) -> anyhow::Result<Option<Bytes>> {
        let maybe_cache_backend = self
            .cache
            .as_ref()
            .map(|v| Some(v.as_ref()))
            .unwrap_or_else(global_cache);

        let cache_key = self.cache_key(sizing_id, image_id, fetch_kind);

        if let Some(cache) = maybe_cache_backend {
            if let Some(buffer) = cache.get(&cache_key) {
                return Ok(Some(buffer));
            }
        }

        let maybe_existing = self
            .storage
            .fetch(self.bucket_id, image_id, fetch_kind, sizing_id)
            .await?;

        if let Some(cache) = maybe_cache_backend {
            if let Some(ref buffer) = maybe_existing {
                cache.insert(cache_key, buffer.clone());
            }
        }

        Ok(maybe_existing)
    }

    async fn concurrent_upload(
        &self,
        image_id: String,
        to_store: Vec<StoreEntry>,
    ) -> anyhow::Result<Vec<ImageUploadInfo>> {
        let mut image_upload_info = vec![];
        let mut tasks = vec![];
        for store_entry in to_store {
            image_upload_info.push(ImageUploadInfo {
                sizing_id: store_entry.sizing_id,
            });
            let storage = self.storage.clone();
            let bucket_id = self.bucket_id;
            let cache = self.cache.clone();
            let image_id = image_id.clone();
            let cache_key = self.cache_key(store_entry.sizing_id, &image_id, store_entry.kind);

            let t = tokio::spawn(async move {
                storage
                    .store(
                        bucket_id,
                        &image_id,
                        store_entry.kind,
                        store_entry.sizing_id,
                        store_entry.data.clone(),
                    )
                    .await?;

                if let Some(ref cache) = cache {
                    cache.insert(cache_key, store_entry.data);
                }

                Ok::<_, anyhow::Error>(())
            });

            tasks.push(t);
        }

        for task in tasks {
            task.await??;
        }

        Ok(image_upload_info)
    }
}
