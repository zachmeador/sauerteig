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
    /// Time from the first shadow page pass starting to the last ending, in the
    /// same frame as `latest`. Passes can overlap, so this is not their sum.
    pub latest_shadow_span: Option<f64>,
    /// Time from the first timed pass of the frame starting to the last ending:
    /// what the GPU took for the frame, as far as the passes show.
    pub latest_span: Option<f64>,
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
            latest_shadow_span: None,
            latest_span: None,
        }
    }
    pub fn supported(&self) -> bool {
        self.supported
    }
    /// The serial of the most recently submitted timed frame.
    pub fn serial(&self) -> u64 {
        self.serial
    }
    /// The serial of the frame `latest` describes.
    pub fn latest_serial(&self) -> u64 {
        self.latest_serial
    }
    pub fn collect(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
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
                    let mut span: Option<(u64, u64)> = None;
                    let mut whole: Option<(u64, u64)> = None;
                    for (i, label) in slot.labels.iter().enumerate() {
                        if values[i * 2] <= values[i * 2 + 1] {
                            let (start, end) = (values[i * 2], values[i * 2 + 1]);
                            whole = Some(
                                whole.map_or((start, end), |(s, e)| (s.min(start), e.max(end))),
                            );
                        }
                        if label.starts_with("shadow page") && values[i * 2] <= values[i * 2 + 1] {
                            let (start, end) = (values[i * 2], values[i * 2 + 1]);
                            span = Some(
                                span.map_or((start, end), |(s, e)| (s.min(start), e.max(end))),
                            );
                        }
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
                        self.latest_shadow_span = span
                            .map(|(start, end)| (end - start) as f64 * self.period / 1_000_000.0);
                        self.latest_span = whole
                            .map(|(start, end)| (end - start) as f64 * self.period / 1_000_000.0);
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
    /// Claim the next pair of timestamps for a pass of this frame.
    fn claim(&mut self, label: &'static str) -> Option<(&wgpu::QuerySet, u32)> {
        let slot = self.active?;
        let index = self.labels.len() as u32 * 2;
        if index + 1 >= QUERIES {
            return None;
        }
        self.labels.push(label);
        Some((self.slots[slot].queries.as_ref()?, index))
    }
    pub fn pass(&mut self, label: &'static str) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        let (query_set, index) = self.claim(label)?;
        Some(wgpu::RenderPassTimestampWrites {
            query_set,
            beginning_of_pass_write_index: Some(index),
            end_of_pass_write_index: Some(index + 1),
        })
    }
    pub fn compute_pass(
        &mut self,
        label: &'static str,
    ) -> Option<wgpu::ComputePassTimestampWrites<'_>> {
        let (query_set, index) = self.claim(label)?;
        Some(wgpu::ComputePassTimestampWrites {
            query_set,
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
            queue.on_submitted_work_done(move || rendered.store(true, Ordering::Release));
        }
    }
}
