//! Optional pass timestamps with a bounded asynchronous readback ring. Query
//! resolution uses a later queue submission, after a completion callback on
//! native platforms. This avoids stale final-pass counters on Metal without
//! waiting on the CPU during play.
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

const QUERIES: u32 = 128;
type Completion = Arc<Mutex<Option<Result<(), wgpu::BufferAsyncError>>>>;
struct Readback {
    queries: Option<wgpu::QuerySet>,
    buffer: wgpu::Buffer,
    rendered: Arc<AtomicBool>,
    completion: Completion,
    labels: Vec<&'static str>,
    pending: bool,
    serial: u64,
}

pub(super) struct GpuTiming {
    supported: bool,
    resolve: wgpu::Buffer,
    slots: Vec<Readback>,
    active: Option<usize>,
    labels: Vec<&'static str>,
    period: f64,
    serial: u64,
    latest_serial: u64,
    pub latest: BTreeMap<String, f64>,
}
impl GpuTiming {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let supported = device.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        Self {
            supported,
            resolve: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("timestamp resolve"),
                size: QUERIES as u64 * 8,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            slots: (0..3)
                .map(|_| Readback {
                    queries: supported.then(|| {
                        device.create_query_set(&wgpu::QuerySetDescriptor {
                            label: Some("frame timestamps"),
                            ty: wgpu::QueryType::Timestamp,
                            count: QUERIES,
                        })
                    }),
                    buffer: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("timestamp readback"),
                        size: QUERIES as u64 * 8,
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    }),
                    rendered: Arc::new(AtomicBool::new(false)),
                    completion: Arc::new(Mutex::new(None)),
                    labels: Vec::new(),
                    pending: false,
                    serial: 0,
                })
                .collect(),
            active: None,
            labels: Vec::new(),
            period: queue.get_timestamp_period() as f64,
            serial: 0,
            latest_serial: 0,
            latest: BTreeMap::new(),
        }
    }
    pub fn supported(&self) -> bool {
        self.supported
    }
    pub fn collect(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        #[cfg(not(target_arch = "wasm32"))]
        device.poll(wgpu::Maintain::Poll);
        for slot in &mut self.slots {
            if slot.rendered.swap(false, Ordering::AcqRel) {
                let count = slot.labels.len() as u32 * 2;
                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("completed frame timings"),
                });
                encoder.resolve_query_set(
                    slot.queries.as_ref().unwrap(),
                    0..count,
                    &self.resolve,
                    0,
                );
                encoder.copy_buffer_to_buffer(&self.resolve, 0, &slot.buffer, 0, count as u64 * 8);
                queue.submit([encoder.finish()]);
                let completion = slot.completion.clone();
                slot.buffer
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |result| {
                        *completion.lock().unwrap() = Some(result);
                    });
            }
            if let Some(result) = slot.completion.lock().unwrap().take() {
                if result.is_ok() {
                    let data = slot.buffer.slice(..).get_mapped_range();
                    let values: &[u64] = bytemuck::cast_slice(&data);
                    let mut sample = BTreeMap::new();
                    for (i, label) in slot.labels.iter().enumerate() {
                        // Reject invalid/disjoint counters instead of reporting a
                        // misleading zero or enormous elapsed time.
                        if let Some(ticks) = values[i * 2 + 1].checked_sub(values[i * 2]) {
                            sample.insert(
                                (*label).to_owned(),
                                ticks as f64 * self.period / 1_000_000.0,
                            );
                        }
                    }
                    if slot.serial >= self.latest_serial {
                        self.latest = sample;
                        self.latest_serial = slot.serial;
                    }
                    drop(data);
                    slot.buffer.unmap();
                }
                slot.pending = false;
            }
        }
    }
    pub fn begin(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.collect(device, queue);
        self.labels.clear();
        self.active = self
            .supported
            .then(|| self.slots.iter().position(|s| !s.pending))
            .flatten();
    }
    pub fn pass(&mut self, label: &'static str) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        let slot = self.active?;
        let index = self.labels.len() as u32 * 2;
        if index + 1 >= QUERIES {
            return None;
        }
        self.labels.push(label);
        Some(wgpu::RenderPassTimestampWrites {
            query_set: self.slots[slot].queries.as_ref()?,
            beginning_of_pass_write_index: Some(index),
            end_of_pass_write_index: Some(index + 1),
        })
    }
    pub fn submitted(&mut self, queue: &wgpu::Queue) {
        if let Some(index) = self.active.take() {
            let slot = &mut self.slots[index];
            slot.pending = true;
            self.serial += 1;
            slot.serial = self.serial;
            slot.labels.clone_from(&self.labels);
            let rendered = slot.rendered.clone();
            #[cfg(not(target_arch = "wasm32"))]
            queue.on_submitted_work_done(move || rendered.store(true, Ordering::Release));
            // wgpu 24's WebGPU backend has no completion callback implementation.
            // Resolve on the next frame's queue submission; WebGPU orders query
            // writes and resolution, and map_async handles readback completion.
            #[cfg(target_arch = "wasm32")]
            {
                let _ = queue;
                rendered.store(true, Ordering::Release);
            }
        }
    }
}
