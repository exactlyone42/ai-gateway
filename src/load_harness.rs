#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        fs,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };

    use chrono::Utc;
    use futures_util::{
        FutureExt as _,
        future::{join_all, select_all},
    };
    use serde_json::{Value, json};
    use tokio::sync::Barrier;
    use uuid::Uuid;

    use crate::{
        adapters::stream::{CrossStreamObserver, OpenAiResponsesObserver},
        canonical::CanonicalStreamEvent,
        model_registry::GatewayOperation,
        policy::WorkloadClass,
        scheduler::{
            Scheduler, SchedulerConfig, SchedulerError, SchedulerRequest, effective_workload_weight,
        },
        services::streaming::{EventDecoder, StreamObserver},
    };

    const STREAMS: usize = 100;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn w18_in_process_load_harness() {
        let stream_first = stream_batch().await;
        let stream_second = stream_batch().await;
        let stream = stream_batch().await;
        let scheduler = scheduler_batch().await;
        let faults = fault_matrix();

        assert_eq!(stream["started"], STREAMS);
        assert_eq!(stream["completed"], 90);
        assert_eq!(stream["cancelled"], 10);
        assert_eq!(stream["unique_response_ids"], STREAMS);
        assert_eq!(stream["response_id_mismatches"], 0);
        assert_eq!(stream["peak_simultaneous_streams"], STREAMS);
        assert_eq!(stream["active_after"], 0);
        assert_eq!(scheduler["served"], 100);
        assert!(scheduler["provider_peak"].as_u64().unwrap_or(u64::MAX) <= 3);
        assert_eq!(scheduler["provider_limit_negative_control_peak"], 6);
        assert_eq!(scheduler["background_served"], 60);
        assert!(
            scheduler["interactive_first_20"]
                .as_u64()
                .unwrap_or_default()
                > 8
        );
        assert_eq!(scheduler["reservation_negative_control"], true);
        assert_eq!(scheduler["deadline_rejected"], true);
        assert_eq!(scheduler["queue_full_rejected"], true);
        assert_eq!(
            scheduler["aging_enabled_batch_weight_after_20_intervals"],
            20
        );
        assert_eq!(
            scheduler["aging_disabled_batch_weight_after_20_intervals"],
            1
        );
        assert!(
            scheduler["aging_enabled_batch_position"].as_u64()
                < scheduler["aging_disabled_batch_position"].as_u64()
        );
        assert_eq!(faults["malformed_rejected"], true);
        assert_eq!(faults["partial_rejected"], true);
        assert_eq!(faults["tool_call_completed"], true);
        assert_eq!(faults["shared_stream_state_rejected"], true);
        assert_eq!(faults["oversize_rejected"], true);
        let stream_cycles_rss_after_kib = [
            stream_first["rss_after_kib"].clone(),
            stream_second["rss_after_kib"].clone(),
            stream["rss_after_kib"].clone(),
        ];

        println!(
            "W18_MEASUREMENT={}",
            json!({
                "schema": "w18_in_process_v1",
                "stream": stream,
                "stream_cycles_rss_after_kib": stream_cycles_rss_after_kib,
                "scheduler": scheduler,
                "faults": faults,
            })
        );
    }

    async fn stream_batch() -> Value {
        let before_rss = resident_kib("VmRSS");
        let before_hwm = resident_kib("VmHWM");
        let before_fds = fd_count();
        let cpu_start = process_cpu_ns();
        let wall_start = Instant::now();
        let start = Arc::new(Barrier::new(STREAMS + 1));
        let overlap = Arc::new(Barrier::new(STREAMS + 1));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let tasks = (0..STREAMS)
        .map(|index| {
            let start = Arc::clone(&start);
            let overlap = Arc::clone(&overlap);
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            tokio::spawn(async move {
                start.wait().await;
                let response_id = format!("resp_load_{index:03}");
                let created = format!(
                    "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"status\":\"in_progress\"}},\"sequence_number\":0}}\n\n"
                );
                let completed = format!(
                    "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"{response_id}\",\"status\":\"completed\",\"output\":[],\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}},\"sequence_number\":1}}\n\n"
                );
                let split = created.len() / 2;
                let mut decoder = EventDecoder::new(4096);
                let mut observer = OpenAiResponsesObserver::default();
                let mut observed_id = None;
                let entered = Instant::now();
                assert!(decoder.feed(&created.as_bytes()[..split]).unwrap().is_empty());
                for raw in decoder.feed(&created.as_bytes()[split..]).unwrap() {
                    for event in observer.observe(&raw).unwrap() {
                        if let CanonicalStreamEvent::ResponseStarted(id) = event {
                            observed_id = Some(id);
                        }
                    }
                }
                let first_event_us = entered.elapsed().as_micros();
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                update_peak(&peak, current);
                let _guard = ActiveGuard(active);
                overlap.wait().await;
                if index % 10 == 0 {
                    return StreamResult {
                        response_id: observed_id,
                        expected_id: response_id,
                        first_event_us,
                        completed: false,
                        cancelled: true,
                    };
                }
                if index % 7 == 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                } else {
                    tokio::task::yield_now().await;
                }
                let completed = decoder
                    .feed(completed.as_bytes())
                    .unwrap()
                    .into_iter()
                    .flat_map(|raw| observer.observe(&raw).unwrap())
                    .any(|event| matches!(event, CanonicalStreamEvent::ResponseCompleted));
                decoder.finish().unwrap();
                StreamResult {
                    response_id: observed_id,
                    expected_id: response_id,
                    first_event_us,
                    completed,
                    cancelled: false,
                }
            })
        })
        .collect::<Vec<_>>();
        start.wait().await;
        overlap.wait().await;
        let results = join_all(tasks)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect::<Vec<_>>();

        let wall_us = wall_start.elapsed().as_micros();
        let cpu_ns = process_cpu_ns().saturating_sub(cpu_start);
        let after_rss = resident_kib("VmRSS");
        let after_hwm = resident_kib("VmHWM");
        let first_event = results
            .iter()
            .map(|value| value.first_event_us)
            .collect::<Vec<_>>();
        let ids = results
            .iter()
            .filter_map(|value| value.response_id.clone())
            .collect::<HashSet<_>>();
        let cancelled = results.iter().filter(|value| value.cancelled).count();
        let completed = results.iter().filter(|value| value.completed).count();
        let response_id_mismatches = results
            .iter()
            .filter(|value| value.response_id.as_deref() != Some(value.expected_id.as_str()))
            .count();
        json!({
            "started": STREAMS,
            "completed": completed,
            "cancelled": cancelled,
            "unique_response_ids": ids.len(),
            "response_id_mismatches": response_id_mismatches,
            "peak_simultaneous_streams": peak.load(Ordering::SeqCst),
            "active_after": active.load(Ordering::SeqCst),
            "wall_us": wall_us,
            "cpu_ns": cpu_ns,
            "first_event_us": percentiles(first_event),
            "rss_before_kib": before_rss,
            "rss_after_kib": after_rss,
            "rss_delta_kib": after_rss.saturating_sub(before_rss),
            "process_historical_hwm_delta_kib": after_hwm.saturating_sub(before_hwm),
            "process_historical_hwm_delta_per_stream_kib": after_hwm.saturating_sub(before_hwm).div_ceil(STREAMS as u64),
            "fd_delta": fd_count() as i64 - before_fds as i64,
        })
    }

    async fn scheduler_batch() -> Value {
        let provider = Uuid::new_v4();
        let config = SchedulerConfig {
            active_interactive_reserved: 2,
            active_shared: 2,
            active_background_reserved: 2,
            queue_interactive_reserved: 50,
            queue_shared: 100,
            queue_background_reserved: 50,
            queue_per_tenant: 100,
            queue_per_credential: 100,
            per_provider: 3,
            per_model: 100,
            per_user: 100,
            per_device: 100,
            per_client: 100,
            per_agent: 100,
            per_credential: 100,
            per_workload: 100,
            hard_queue_timeout: Duration::from_secs(5),
            aging_interval: Duration::from_secs(1),
            aging_enabled: true,
            reservations_enabled: true,
        };
        let scheduler = Scheduler::new(config.clone()).unwrap();
        let interactive = scheduler_request(provider, WorkloadClass::Interactive, "premium");
        let background = scheduler_request(provider, WorkloadClass::Background, "premium");
        let blockers = [
            scheduler.try_acquire(&interactive).unwrap().unwrap(),
            scheduler.try_acquire(&interactive).unwrap().unwrap(),
            scheduler.try_acquire(&background).unwrap().unwrap(),
        ];
        let specifications = (0..100)
            .map(|index| match index % 10 {
                0 => (WorkloadClass::Interactive, "premium"),
                1..=3 => (WorkloadClass::Interactive, "economy"),
                4..=6 => (WorkloadClass::Background, "premium"),
                _ => (WorkloadClass::Batch, "economy"),
            })
            .collect::<Vec<_>>();
        let entered = Instant::now();
        let tickets = specifications
            .iter()
            .map(|(workload, quality)| {
                scheduler
                    .enqueue(scheduler_request(provider, *workload, quality))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let start = Arc::new(Barrier::new(101));
        let order = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let tasks = tickets
            .into_iter()
            .zip(specifications)
            .map(|(ticket, (workload, quality))| {
                let start = Arc::clone(&start);
                let order = Arc::clone(&order);
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak);
                tokio::spawn(async move {
                    start.wait().await;
                    let lease = ticket.wait().await.unwrap();
                    let wait_us = entered.elapsed().as_micros();
                    let position = order.fetch_add(1, Ordering::SeqCst);
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    update_peak(&peak, current);
                    tokio::time::sleep(Duration::from_micros(200)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    drop(lease);
                    ScheduleResult {
                        position,
                        workload,
                        quality,
                        wait_us,
                    }
                })
            })
            .collect::<Vec<_>>();
        start.wait().await;
        drop(blockers);
        let results = join_all(tasks)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        let interactive_first_20 = results
            .iter()
            .filter(|value| value.position < 20 && is_interactive(value.workload))
            .count();
        let background = results
            .iter()
            .filter(|value| !is_interactive(value.workload))
            .collect::<Vec<_>>();
        let background_served = background.len();
        let last_background_position = background
            .iter()
            .map(|value| value.position)
            .max()
            .unwrap_or_default();
        let wait_us = results
            .iter()
            .map(|value| value.wait_us)
            .collect::<Vec<_>>();
        let premium_served = results
            .iter()
            .filter(|value| value.quality == "premium")
            .count();
        let aging_enabled = effective_workload_weight(&config, WorkloadClass::Batch, 20);
        let mut aging_disabled_config = config.clone();
        aging_disabled_config.aging_enabled = false;
        let aging_disabled =
            effective_workload_weight(&aging_disabled_config, WorkloadClass::Batch, 20);
        let aging_enabled_batch_position = aging_dispatch_position(provider, true).await;
        let aging_disabled_batch_position = aging_dispatch_position(provider, false).await;

        json!({
            "submitted": 100,
            "served": results.len(),
            "interactive_first_20": interactive_first_20,
            "background_served": background_served,
            "last_background_position": last_background_position,
            "premium_served_as_label_only": premium_served,
            "provider_limit": 3,
            "provider_peak": peak.load(Ordering::SeqCst),
            "provider_limit_negative_control_peak": provider_limit_negative_control(provider),
            "wait_us": percentiles(wait_us),
            "reservation_negative_control": reservation_negative_control(provider),
            "aging_enabled_batch_weight_after_20_intervals": aging_enabled,
            "aging_disabled_batch_weight_after_20_intervals": aging_disabled,
            "aging_enabled_batch_position": aging_enabled_batch_position,
            "aging_disabled_batch_position": aging_disabled_batch_position,
            "deadline_rejected": deadline_rejected(provider),
            "queue_full_rejected": queue_full_rejected(provider),
        })
    }

    fn fault_matrix() -> Value {
        let mut malformed = OpenAiResponsesObserver::default();
        let malformed_rejected = malformed.observe(b"data: not-json\n\n").is_err();
        let mut partial = EventDecoder::new(4096);
        let partial_rejected = partial
            .feed(b"data: {\"type\":\"response.created\"}")
            .is_ok_and(|events| events.is_empty())
            && partial.finish().is_err();
        let oversize_rejected = EventDecoder::new(8).feed(b"data: 123\n\n").is_err();
        let mut shared = OpenAiResponsesObserver::default();
        shared
        .observe(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_a\",\"object\":\"response\",\"status\":\"in_progress\"},\"sequence_number\":0}\n\n")
        .unwrap();
        let shared_stream_state_rejected = shared
        .observe(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_b\",\"object\":\"response\",\"status\":\"in_progress\"},\"sequence_number\":0}\n\n")
        .is_err();
        let mut tool = CrossStreamObserver::new(GatewayOperation::AnthropicMessages).unwrap();
        let frames = [
        b"data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"usage\":{\"input_tokens\":1}}}\n\n".as_slice(),
        b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_1\",\"name\":\"lookup\",\"input\":{}}}\n\n".as_slice(),
        b"data: {\"type\":\"content_block_stop\",\"index\":0}\n\n".as_slice(),
        b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":1}}\n\n".as_slice(),
        b"data: {\"type\":\"message_stop\"}\n\n".as_slice(),
    ];
        let tool_call_completed = frames
            .into_iter()
            .flat_map(|frame| tool.observe(frame).unwrap())
            .any(|event| matches!(event, CanonicalStreamEvent::ResponseCompleted));
        json!({
            "malformed_rejected": malformed_rejected,
            "partial_rejected": partial_rejected,
            "oversize_rejected": oversize_rejected,
            "shared_stream_state_rejected": shared_stream_state_rejected,
            "tool_call_completed": tool_call_completed,
        })
    }

    fn reservation_negative_control(provider: Uuid) -> bool {
        let enabled = Scheduler::new(SchedulerConfig {
            active_interactive_reserved: 1,
            active_shared: 1,
            active_background_reserved: 1,
            per_provider: 3,
            ..SchedulerConfig::default()
        })
        .unwrap();
        let background = scheduler_request(provider, WorkloadClass::Background, "economy");
        let _enabled_one = enabled.try_acquire(&background).unwrap().unwrap();
        let _enabled_two = enabled.try_acquire(&background).unwrap().unwrap();
        let interactive = scheduler_request(provider, WorkloadClass::Interactive, "economy");
        let enabled_protects = enabled.try_acquire(&interactive).unwrap().is_some();

        let disabled = Scheduler::new(SchedulerConfig {
            active_interactive_reserved: 1,
            active_shared: 1,
            active_background_reserved: 1,
            per_provider: 3,
            reservations_enabled: false,
            ..SchedulerConfig::default()
        })
        .unwrap();
        let _disabled_one = disabled.try_acquire(&background).unwrap().unwrap();
        let _disabled_two = disabled.try_acquire(&background).unwrap().unwrap();
        let _disabled_three = disabled.try_acquire(&background).unwrap().unwrap();
        enabled_protects && disabled.try_acquire(&interactive).unwrap().is_none()
    }

    fn provider_limit_negative_control(provider: Uuid) -> usize {
        let scheduler = Scheduler::new(SchedulerConfig {
            active_interactive_reserved: 2,
            active_shared: 2,
            active_background_reserved: 2,
            per_provider: 6,
            per_model: 100,
            per_user: 100,
            per_device: 100,
            per_client: 100,
            per_agent: 100,
            per_credential: 100,
            per_workload: 100,
            ..SchedulerConfig::default()
        })
        .unwrap();
        let interactive = scheduler_request(provider, WorkloadClass::Interactive, "economy");
        let background = scheduler_request(provider, WorkloadClass::Background, "economy");
        let leases = [
            scheduler.try_acquire(&interactive).unwrap().unwrap(),
            scheduler.try_acquire(&interactive).unwrap().unwrap(),
            scheduler.try_acquire(&interactive).unwrap().unwrap(),
            scheduler.try_acquire(&interactive).unwrap().unwrap(),
            scheduler.try_acquire(&background).unwrap().unwrap(),
            scheduler.try_acquire(&background).unwrap().unwrap(),
        ];
        leases.len()
    }

    async fn aging_dispatch_position(provider: Uuid, aging_enabled: bool) -> usize {
        let scheduler = Scheduler::new(SchedulerConfig {
            active_interactive_reserved: 1,
            active_shared: 1,
            active_background_reserved: 1,
            queue_interactive_reserved: 20,
            queue_shared: 20,
            queue_background_reserved: 20,
            queue_per_tenant: 20,
            queue_per_credential: 20,
            per_provider: 1,
            per_model: 20,
            per_user: 20,
            per_device: 20,
            per_client: 20,
            per_agent: 20,
            per_credential: 20,
            per_workload: 20,
            aging_interval: Duration::from_millis(1),
            aging_enabled,
            ..SchedulerConfig::default()
        })
        .unwrap();
        let blocker = scheduler_request(provider, WorkloadClass::Interactive, "economy");
        let blocker = scheduler.try_acquire(&blocker).unwrap().unwrap();
        let tickets = (0..11)
            .map(|index| {
                let workload = if index == 10 {
                    WorkloadClass::Batch
                } else {
                    WorkloadClass::Interactive
                };
                scheduler
                    .enqueue(scheduler_request(provider, workload, "economy"))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        tokio::time::sleep(Duration::from_millis(25)).await;
        drop(blocker);
        let mut pending = tickets
            .into_iter()
            .enumerate()
            .map(|(index, ticket)| async move { (index, ticket.wait().await) }.boxed())
            .collect::<Vec<_>>();
        let mut position = 0;
        let mut batch_position = usize::MAX;
        while !pending.is_empty() {
            let ((index, lease), _, remaining) = select_all(pending).await;
            pending = remaining;
            let lease = lease.unwrap();
            if index == 10 {
                batch_position = position;
            }
            position += 1;
            drop(lease);
        }
        batch_position
    }

    fn deadline_rejected(provider: Uuid) -> bool {
        let scheduler = Scheduler::new(SchedulerConfig::default()).unwrap();
        let mut request = scheduler_request(provider, WorkloadClass::Interactive, "economy");
        request.received_at = Utc::now() - chrono::Duration::minutes(1);
        matches!(
            scheduler.try_acquire(&request),
            Err(SchedulerError::Expired)
        )
    }

    fn queue_full_rejected(provider: Uuid) -> bool {
        let scheduler = Scheduler::new(SchedulerConfig {
            active_interactive_reserved: 1,
            active_shared: 1,
            active_background_reserved: 1,
            queue_interactive_reserved: 1,
            queue_shared: 1,
            queue_background_reserved: 1,
            queue_per_tenant: 1,
            queue_per_credential: 1,
            per_provider: 1,
            ..SchedulerConfig::default()
        })
        .unwrap();
        let request = scheduler_request(provider, WorkloadClass::Interactive, "economy");
        let _active = scheduler.try_acquire(&request).unwrap().unwrap();
        let _queued = scheduler.enqueue(request.clone()).unwrap();
        matches!(scheduler.enqueue(request), Err(SchedulerError::QueueFull))
    }

    fn scheduler_request(
        provider_id: Uuid,
        workload: WorkloadClass,
        quality: &str,
    ) -> SchedulerRequest {
        SchedulerRequest {
            tenant_id: Uuid::new_v4(),
            provider_id,
            model_id: Uuid::new_v4(),
            user_id: Some(Uuid::new_v4()),
            device_id: Some(Uuid::new_v4()),
            client_id: Some(Uuid::new_v4()),
            agent_id: Some(Uuid::new_v4()),
            credential_id: Uuid::new_v4(),
            workload,
            quality: quality.to_owned(),
            received_at: Utc::now(),
            policy_deadline: None,
            cache_affinity: None,
            quota: Vec::new(),
        }
    }

    fn is_interactive(workload: WorkloadClass) -> bool {
        matches!(
            workload,
            WorkloadClass::Critical | WorkloadClass::Interactive | WorkloadClass::Foreground
        )
    }

    fn percentiles(mut values: Vec<u128>) -> Value {
        values.sort_unstable();
        let at = |percent: usize| values[(values.len() - 1) * percent / 100];
        json!({"p50": at(50), "p95": at(95), "p99": at(99)})
    }

    fn update_peak(peak: &AtomicUsize, current: usize) {
        let mut observed = peak.load(Ordering::Relaxed);
        while current > observed {
            match peak.compare_exchange_weak(observed, current, Ordering::SeqCst, Ordering::Relaxed)
            {
                Ok(_) => return,
                Err(next) => observed = next,
            }
        }
    }

    struct ActiveGuard(Arc<AtomicUsize>);

    impl Drop for ActiveGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct StreamResult {
        response_id: Option<String>,
        expected_id: String,
        first_event_us: u128,
        completed: bool,
        cancelled: bool,
    }

    struct ScheduleResult {
        position: usize,
        workload: WorkloadClass,
        quality: &'static str,
        wait_us: u128,
    }

    fn resident_kib(field: &str) -> u64 {
        fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status.lines().find_map(|line| {
                    line.strip_prefix(field)
                        .and_then(|value| value.strip_prefix(':'))
                        .and_then(|value| value.split_whitespace().next())
                        .and_then(|value| value.parse().ok())
                })
            })
            .unwrap_or_default()
    }

    fn fd_count() -> usize {
        fs::read_dir("/proc/self/fd")
            .map(|entries| entries.filter_map(Result::ok).count())
            .unwrap_or_default()
    }

    fn process_cpu_ns() -> u128 {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `time` is a valid writable timespec and CLOCK_PROCESS_CPUTIME_ID
        // does not retain its pointer.
        let result = unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) };
        if result != 0 {
            return 0;
        }
        (time.tv_sec as u128)
            .saturating_mul(1_000_000_000)
            .saturating_add(time.tv_nsec as u128)
    }
}
