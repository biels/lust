use crate::config::ImageKind;
use async_trait::async_trait;
use bytes::Bytes;

#[async_trait]
pub trait StorageBackend: Sync + Send + 'static {
    async fn store(
        &self,
        bucket_id: u32,
        image_id: &str,
        kind: ImageKind,
        sizing_id: u32,
        data: Bytes,
    ) -> anyhow::Result<()>;

    async fn fetch(
        &self,
        bucket_id: u32,
        image_id: &str,
        kind: ImageKind,
        sizing_id: u32,
    ) -> anyhow::Result<Option<Bytes>>;

    async fn delete(&self, bucket_id: u32, image_id: &str)
        -> anyhow::Result<Vec<(u32, ImageKind)>>;
}
