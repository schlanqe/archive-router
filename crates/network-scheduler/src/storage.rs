use std::fmt::Display;
use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use aws_sdk_s3 as s3;
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::types::Object;
use itertools::Itertools;
use nonempty::NonEmpty;
use subsquid_network_transport::task_manager::{CancellationToken, TaskManager};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::Mutex;

use crate::cli::Config;
use crate::data_chunk::DataChunk;
use crate::scheduler::Scheduler;
use crate::scheduling_unit::{bundle_chunks, SchedulingUnit};

#[derive(Clone)]
struct DatasetStorage {
    bucket: String,
    client: s3::Client,
    last_key: Option<String>,
    last_block: Option<u32>,
}

#[derive(Debug, Clone)]
struct S3Object {
    prefix: String,
    // common prefix identifies all objects belonging to a single chunk
    file_name: String,
    size: u64,
}

impl S3Object {
    fn key(&self) -> String {
        format!("{}/{}", self.prefix, self.file_name)
    }
}

impl TryFrom<Object> for S3Object {
    type Error = &'static str;

    fn try_from(obj: Object) -> Result<Self, Self::Error> {
        let key = obj.key.ok_or("Object key missing")?;
        let (prefix, file_name) = match key.rsplit_once('/') {
            Some((prefix, file_name)) => (prefix.to_string(), file_name.to_string()),
            None => return Err("Invalid key (no prefix)"),
        };
        let size = obj.size.unwrap_or_default() as u64;
        Ok(Self {
            prefix,
            file_name,
            size,
        })
    }
}

impl DatasetStorage {
    pub fn new(bucket: impl ToString, client: s3::Client) -> Self {
        Self {
            bucket: bucket.to_string(),
            client,
            last_key: None,
            last_block: None,
        }
    }

    async fn list_all_new_objects(&self) -> anyhow::Result<Vec<S3Object>> {
        let mut last_key = self.last_key.clone();
        let mut result = Vec::new();
        loop {
            let s3_result = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .set_start_after(last_key)
                .send()
                .await?;
            let objects: NonEmpty<S3Object> = match s3_result
                .contents
                .and_then(|objects| {
                    objects
                        .into_iter()
                        .map(TryInto::try_into)
                        .collect::<Result<Vec<S3Object>, &'static str>>()
                        .ok()
                })
                .and_then(NonEmpty::from_vec)
            {
                Some(objects) => objects,
                None => break,
            };
            last_key = Some(objects.last().key());
            result.extend(objects)
        }

        Ok(result)
    }

    async fn list_all_new_chunks(&mut self) -> anyhow::Result<Vec<DataChunk>> {
        let objects = match NonEmpty::from_vec(self.list_all_new_objects().await?) {
            None => return Ok(Vec::new()),
            Some(objects) => objects,
        };
        let last_key = objects.last().key();
        let chunks = objects
            .into_iter()
            .group_by(|obj| obj.prefix.clone())
            .into_iter()
            .map(|(_, objects)| self.objects_to_chunk(objects))
            .collect::<anyhow::Result<Vec<DataChunk>>>()?;

        // Verify if chunks are continuous
        let mut next_block = self.last_block.map(|x| x + 1).unwrap_or_default();
        for chunk in chunks.iter() {
            if chunk.block_range.begin != next_block {
                anyhow::bail!(
                    "Blocks {} to {} missing from {}",
                    next_block,
                    chunk.block_range.begin - 1,
                    self.bucket
                );
            }
            next_block = chunk.block_range.end + 1;
        }

        self.last_key = Some(last_key);
        self.last_block = chunks.last().map(|c| c.block_range.end);
        Ok(chunks)
    }

    fn objects_to_chunk(
        &self,
        objs: impl IntoIterator<Item = S3Object>,
    ) -> anyhow::Result<DataChunk> {
        let mut last_key = None;
        let mut blocks_file_present = false;
        let mut size_bytes = 0u64;
        for obj in objs {
            last_key = Some(obj.key());
            blocks_file_present |= obj.file_name == "blocks.parquet";
            size_bytes += obj.size;
        }
        let last_key = match last_key {
            Some(key) => key,
            None => anyhow::bail!("Empty object group"),
        };
        if !blocks_file_present {
            anyhow::bail!("blocks.parquet missing")
        }
        let chunk = match DataChunk::new(&self.bucket, &last_key, size_bytes) {
            Ok(chunk) => chunk,
            Err(_) => anyhow::bail!("Invalid data chunk"),
        };
        log::debug!("Downloaded chunk {chunk:?}");

        Ok(chunk)
    }

    pub async fn get_incoming_chunks(
        mut self,
        sender: Sender<NonEmpty<DataChunk>>,
        cancel_token: CancellationToken,
    ) {
        log::info!("Reading chunks from bucket {}", self.bucket);
        loop {
            let chunks_res = tokio::select! {
                res = self.list_all_new_chunks() => res,
                _ = cancel_token.cancelled() => break,
            };
            let chunks = match chunks_res {
                Ok(chunks) => chunks,
                Err(e) => {
                    log::error!("Error getting data chunks: {e:?}");
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(60)) => continue,
                        _ = cancel_token.cancelled() => break,
                    }
                }
            };
            let chunks = match NonEmpty::from_vec(chunks) {
                Some(chunks) => chunks,
                None => {
                    log::info!("Now more chunks for now. Waiting...");
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(300)) => continue,
                        _ = cancel_token.cancelled() => break,
                    }
                }
            };
            log::info!(
                "Downloaded {} new chunks from bucket {}",
                chunks.len(),
                self.bucket
            );

            match sender.send(chunks).await {
                Err(_) => break,
                Ok(_) => continue,
            }
        }
        log::info!("Chunks stream ended");
    }
}

#[derive(Clone)]
pub struct S3Storage {
    client: s3::Client,
    config: &'static Config,
    scheduler_state_key: String,
    task_manager: Arc<Mutex<TaskManager>>,
}

impl S3Storage {
    pub async fn new(scheduler_id: impl Display) -> Self {
        let config = Config::get();
        let s3_config = aws_config::from_env()
            .endpoint_url(&config.s3_endpoint)
            .load()
            .await;
        let client = s3::Client::new(&s3_config);
        let scheduler_state_key = format!("scheduler_{scheduler_id}.json");
        Self {
            client,
            config,
            scheduler_state_key,
            task_manager: Default::default(),
        }
    }

    pub async fn get_incoming_units(&self) -> Receiver<SchedulingUnit> {
        let (unit_sender, unit_receiver) = mpsc::channel(100);
        let mut task_manager = self.task_manager.lock().await;
        let scheduling_unit_size = self.config.scheduling_unit_size;
        for bucket in self.config.dataset_buckets.iter() {
            let (chunk_sender, chunk_receiver) = mpsc::channel(100);
            let storage = DatasetStorage::new(bucket, self.client.clone());
            task_manager
                .spawn(|cancel_token| storage.get_incoming_chunks(chunk_sender, cancel_token));
            task_manager.spawn(|cancel_token| {
                bundle_chunks(
                    chunk_receiver,
                    unit_sender.clone(),
                    scheduling_unit_size,
                    cancel_token,
                )
            });
        }
        unit_receiver
    }

    pub async fn load_scheduler(&self) -> anyhow::Result<Scheduler> {
        let api_result = self
            .client
            .get_object()
            .bucket(&self.config.scheduler_state_bucket)
            .key(&self.scheduler_state_key)
            .send()
            .await;
        let bytes = match api_result {
            Ok(res) => res.body.collect().await?.to_vec(),
            Err(SdkError::ServiceError(e)) if e.err().is_no_such_key() => {
                log::warn!("Scheduler state not found. Initializing with blank state.");
                return Ok(Scheduler::default());
            }
            Err(e) => return Err(anyhow::anyhow!(e)),
        };
        let mut scheduler: Scheduler = serde_json::from_slice(&bytes)?;
        // List of datasets could have changed since last run, need to clear deprecated units
        scheduler.clear_deprecated_units();
        Ok(scheduler)
    }

    pub async fn save_scheduler<T: Deref<Target = Scheduler>>(&self, scheduler: T) {
        log::debug!("Saving scheduler state");
        let state = match serde_json::to_vec(scheduler.deref()) {
            Ok(state) => state,
            Err(e) => return log::error!("Error serializing scheduler state: {e:?}"),
        };
        let _ = self
            .client
            .put_object()
            .bucket(&self.config.scheduler_state_bucket)
            .key(&self.scheduler_state_key)
            .body(state.into())
            .send()
            .await
            .map_err(|e| log::error!("Error saving scheduler state: {e:?}"));
    }
}
