use std::{collections::vec_deque::VecDeque, io, time::Duration};

#[cfg(unix)]
use crate::event::InputDiscardStatus;
#[cfg(unix)]
use crate::event::source::unix::UnixInternalEventSource;
#[cfg(windows)]
use crate::event::source::windows::WindowsEventSource;
#[cfg(feature = "event-stream")]
use crate::event::sys::Waker;
use crate::event::{
    filter::Filter,
    internal::InternalEvent,
    source::{EventSource, ReadGoal},
    timeout::PollTimeout,
};

/// Can be used to read `InternalEvent`s.
pub(crate) struct InternalEventReader {
    events: VecDeque<InternalEvent>,
    source: Option<Box<dyn EventSource>>,
    skipped_events: Vec<InternalEvent>,
}

impl Default for InternalEventReader {
    fn default() -> Self {
        #[cfg(windows)]
        let source = WindowsEventSource::new();
        #[cfg(unix)]
        let source = UnixInternalEventSource::new();

        let source = source.ok().map(|x| Box::new(x) as Box<dyn EventSource>);

        InternalEventReader {
            source,
            events: VecDeque::with_capacity(32),
            skipped_events: Vec::with_capacity(32),
        }
    }
}

impl InternalEventReader {
    #[cfg(unix)]
    pub(crate) fn buffer_input(&mut self, input: &[u8]) -> io::Result<()> {
        let source = self
            .source
            .as_mut()
            .ok_or_else(|| io::Error::other("Failed to initialize input reader"))?;
        source.buffer_input(input, &mut self.events);
        Ok(())
    }

    #[cfg(unix)]
    pub(crate) fn discard_buffered_input(&mut self) -> io::Result<InputDiscardStatus> {
        let source = self
            .source
            .as_mut()
            .ok_or_else(|| io::Error::other("Failed to initialize input reader"))?;
        let status = source.discard_buffered_input();
        self.events.clear();
        self.skipped_events.clear();
        Ok(status)
    }

    #[cfg(unix)]
    pub(crate) fn discard_buffered_input_after_poll(&mut self) -> io::Result<InputDiscardStatus> {
        if self
            .source
            .as_ref()
            .is_some_and(|source| source.has_pending_escape())
        {
            return Ok(InputDiscardStatus::ControlSequenceInProgress);
        }
        self.discard_buffered_input()
    }

    #[cfg(all(feature = "event-stream", unix))]
    pub(crate) fn discard_matching(&mut self, matches: fn(&InternalEvent) -> bool) {
        self.events.retain(|event| !matches(event));
        self.skipped_events.retain(|event| !matches(event));
        if let Some(source) = &mut self.source {
            source.discard_matching(matches);
        }
    }

    #[cfg(all(feature = "event-stream", unix))]
    pub(crate) fn can_query(&self) -> bool {
        !self.events.iter().any(InternalEvent::is_palette_report)
            && self
                .source
                .as_ref()
                .is_some_and(|source| source.can_query())
    }

    #[cfg(all(feature = "event-stream", unix))]
    pub(crate) fn prepare_color_query(&mut self) -> bool {
        if !self.can_query() {
            return false;
        }
        self.source
            .as_mut()
            .expect("ready reader")
            .expect_color_replies();
        true
    }

    // Consume input that predates the handoff marker through the same parser.
    // In particular, never write a reply into a partially received paste.
    #[cfg(all(feature = "event-stream", unix))]
    pub(crate) fn prepare_color_handoff(&mut self, timeout: &PollTimeout) -> io::Result<bool> {
        let source = self
            .source
            .as_mut()
            .ok_or_else(|| io::Error::other("Failed to initialize input reader"))?;
        while !source.can_query() {
            if timeout.elapsed() {
                return Ok(false);
            }
            match source.read_until(timeout.leftover(), ReadGoal::EventOrQueryBoundary) {
                Ok(Some(event)) => self.events.push_back(event),
                Ok(None) if source.can_query() => break,
                Ok(None) => return Ok(false),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        // A stale DA1 may be queued behind the last key in the parser.
        self.discard_matching(InternalEvent::is_handoff_report);
        Ok(!timeout.elapsed())
    }

    #[cfg(all(feature = "event-stream", unix))]
    pub(crate) fn finish_color_handoff(&mut self) {
        self.discard_matching(InternalEvent::is_handoff_report);
        if let Some(source) = &mut self.source {
            source.finish_color_handoff();
        }
    }

    /// Returns a `Waker` allowing to wake/force the `poll` method to return `Ok(false)`.
    #[cfg(feature = "event-stream")]
    pub(crate) fn waker(&self) -> Waker {
        self.source.as_ref().expect("reader source not set").waker()
    }

    pub(crate) fn poll<F>(&mut self, timeout: Option<Duration>, filter: &F) -> io::Result<bool>
    where
        F: Filter,
    {
        self.poll_until(timeout, filter, ReadGoal::Event)
    }

    /// True when the caller can make progress toward its read goal.
    pub(crate) fn poll_until(
        &mut self,
        timeout: Option<Duration>,
        filter: &impl Filter,
        goal: ReadGoal,
    ) -> io::Result<bool> {
        for event in &self.events {
            if filter.eval(event) {
                return Ok(true);
            }
        }

        let event_source = match self.source.as_mut() {
            Some(source) => source,
            None => return Err(std::io::Error::other("Failed to initialize input reader")),
        };

        let poll_timeout = PollTimeout::new(timeout);

        loop {
            let maybe_event = match event_source.read_until(poll_timeout.leftover(), goal) {
                Ok(None) if goal.boundary_reached(event_source.as_ref()) => {
                    self.events.extend(self.skipped_events.drain(..));
                    return Ok(true);
                }
                Ok(None) => None,
                Ok(Some(event)) => {
                    if filter.eval(&event) {
                        Some(event)
                    } else {
                        if !filter.discard_unmatched(&event) {
                            self.skipped_events.push(event);
                        }
                        None
                    }
                }
                Err(e) => {
                    // Cancellation can hand the queue directly to a different filter.
                    self.events.extend(self.skipped_events.drain(..));
                    if e.kind() == io::ErrorKind::Interrupted {
                        return Ok(false);
                    }

                    return Err(e);
                }
            };

            if poll_timeout.elapsed() || maybe_event.is_some() {
                self.events.extend(self.skipped_events.drain(..));

                if let Some(event) = maybe_event {
                    self.events.push_back(event);
                    return Ok(true);
                }

                return Ok(false);
            }
        }
    }

    /// Blocks the thread until a valid `InternalEvent` can be read.
    ///
    /// Internally, we use `try_read`, which buffers the events that do not fulfill the filter
    /// conditions to prevent stalling the thread in an infinite loop.
    pub(crate) fn read<F>(&mut self, filter: &F) -> io::Result<InternalEvent>
    where
        F: Filter,
    {
        // blocks the thread until a valid event is found
        loop {
            if let Some(event) = self.try_read(filter) {
                return Ok(event);
            }

            let _ = self.poll(None, filter)?;
        }
    }

    /// Attempts to read the first valid `InternalEvent`.
    ///
    /// This function checks all events in the queue, and stores events that do not match the
    /// filter in a buffer to be added back to the queue after all items have been evaluated. We
    /// must buffer non-fulfilling events because, if added directly back to the queue, they would
    /// result in an infinite loop, rechecking events that have already been evaluated against the
    /// filter.
    pub(crate) fn try_read<F>(&mut self, filter: &F) -> Option<InternalEvent>
    where
        F: Filter,
    {
        // check all events, storing events that do not match the filter in the `skipped_events`
        // buffer to be added back later
        let mut skipped_events = Vec::new();
        let mut result = None;
        while let Some(event) = self.events.pop_front() {
            if filter.eval(&event) {
                result = Some(event);
                break;
            }

            skipped_events.push(event);
        }

        // Skipped events preceded the unread tail; keep them in that order.
        for event in skipped_events.into_iter().rev() {
            self.events.push_front(event);
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::{collections::VecDeque, time::Duration};

    #[cfg(unix)]
    use super::super::filter::CursorPositionFilter;
    use super::{super::Event, EventSource, Filter, InternalEvent, InternalEventReader};

    #[cfg(all(unix, feature = "event-stream"))]
    impl InternalEventReader {
        pub(crate) fn with_test_source(source: impl EventSource + 'static) -> Self {
            Self {
                events: VecDeque::new(),
                source: Some(Box::new(source)),
                skipped_events: Vec::new(),
            }
        }
    }

    #[derive(Debug, Clone)]
    pub(crate) struct InternalEventFilter;

    impl Filter for InternalEventFilter {
        fn eval(&self, _: &InternalEvent) -> bool {
            true
        }
    }

    #[test]
    fn test_poll_fails_without_event_source() {
        let mut reader = InternalEventReader {
            events: VecDeque::new(),
            source: None,
            skipped_events: Vec::with_capacity(32),
        };

        assert!(reader.poll(None, &InternalEventFilter).is_err());
        assert!(
            reader
                .poll(Some(Duration::from_secs(0)), &InternalEventFilter)
                .is_err()
        );
        assert!(
            reader
                .poll(Some(Duration::from_secs(10)), &InternalEventFilter)
                .is_err()
        );
    }

    #[test]
    fn test_poll_returns_true_for_matching_event_in_queue_at_front() {
        let mut reader = InternalEventReader {
            events: vec![InternalEvent::Event(Event::Resize(10, 10))].into(),
            source: None,
            skipped_events: Vec::with_capacity(32),
        };

        assert!(reader.poll(None, &InternalEventFilter).unwrap());
    }

    #[test]
    #[cfg(unix)]
    fn test_poll_returns_true_for_matching_event_in_queue_at_back() {
        let mut reader = InternalEventReader {
            events: vec![
                InternalEvent::Event(Event::Resize(10, 10)),
                InternalEvent::CursorPosition(10, 20),
            ]
            .into(),
            source: None,
            skipped_events: Vec::with_capacity(32),
        };

        assert!(reader.poll(None, &CursorPositionFilter).unwrap());
    }

    #[test]
    fn test_read_returns_matching_event_in_queue_at_front() {
        const EVENT: InternalEvent = InternalEvent::Event(Event::Resize(10, 10));

        let mut reader = InternalEventReader {
            events: vec![EVENT].into(),
            source: None,
            skipped_events: Vec::with_capacity(32),
        };

        assert_eq!(reader.read(&InternalEventFilter).unwrap(), EVENT);
    }

    #[test]
    #[cfg(unix)]
    fn test_read_returns_matching_event_in_queue_at_back() {
        const CURSOR_EVENT: InternalEvent = InternalEvent::CursorPosition(10, 20);

        let mut reader = InternalEventReader {
            events: vec![InternalEvent::Event(Event::Resize(10, 10)), CURSOR_EVENT].into(),
            source: None,
            skipped_events: Vec::with_capacity(32),
        };

        assert_eq!(reader.read(&CursorPositionFilter).unwrap(), CURSOR_EVENT);
    }

    #[test]
    #[cfg(unix)]
    fn test_read_does_not_consume_skipped_event() {
        const SKIPPED_EVENT: InternalEvent = InternalEvent::Event(Event::Resize(10, 10));
        const CURSOR_EVENT: InternalEvent = InternalEvent::CursorPosition(10, 20);

        let mut reader = InternalEventReader {
            events: vec![SKIPPED_EVENT, CURSOR_EVENT].into(),
            source: None,
            skipped_events: Vec::with_capacity(32),
        };

        assert_eq!(reader.read(&CursorPositionFilter).unwrap(), CURSOR_EVENT);
        assert_eq!(reader.read(&InternalEventFilter).unwrap(), SKIPPED_EVENT);
    }

    #[test]
    #[cfg(unix)]
    fn test_try_read_does_not_consume_skipped_event() {
        const SKIPPED_EVENT: InternalEvent = InternalEvent::Event(Event::Resize(10, 10));
        const CURSOR_EVENT: InternalEvent = InternalEvent::CursorPosition(10, 20);

        let mut reader = InternalEventReader {
            events: vec![
                SKIPPED_EVENT,
                CURSOR_EVENT,
                InternalEvent::Event(Event::Resize(30, 40)),
            ]
            .into(),
            source: None,
            skipped_events: Vec::with_capacity(32),
        };

        assert_eq!(
            reader.try_read(&CursorPositionFilter).unwrap(),
            CURSOR_EVENT
        );
        assert_eq!(
            reader.try_read(&InternalEventFilter).unwrap(),
            SKIPPED_EVENT
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_buffer_input_preserves_events_around_terminal_responses() {
        let input = [
            InternalEvent::Event(Event::Resize(10, 10)),
            InternalEvent::CursorPosition(4, 8),
            InternalEvent::Event(Event::Resize(20, 20)),
        ];
        let mut reader = InternalEventReader {
            events: VecDeque::new(),
            source: Some(Box::new(FakeSource::with_events(&input))),
            skipped_events: Vec::with_capacity(32),
        };

        reader.buffer_input(b"raw terminal input").unwrap();

        assert_eq!(
            reader.events,
            VecDeque::from([
                InternalEvent::Event(Event::Resize(10, 10)),
                InternalEvent::Event(Event::Resize(20, 20)),
            ])
        );
    }

    #[test]
    #[cfg(unix)]
    fn ordinary_input_discards_unsolicited_color_scheme_reports() {
        let key = InternalEvent::Event(Event::Key(crate::event::KeyCode::Char('x').into()));
        let source = FakeSource::with_events(&[
            InternalEvent::ColorSchemeChanged,
            InternalEvent::ColorSchemeReporting(2),
            key.clone(),
        ]);
        let mut reader = InternalEventReader {
            skipped_events: Vec::new(),
            events: VecDeque::new(),
            source: Some(Box::new(source)),
        };
        assert_eq!(
            reader.read(&crate::event::filter::EventFilter).unwrap(),
            key
        );
        assert_eq!(reader.try_read(&InternalEventFilter), None);
    }

    #[cfg(all(unix, feature = "event-stream"))]
    #[test]
    fn handoff_clears_old_markers_and_preserves_input_order() {
        let first = InternalEvent::Event(Event::Resize(10, 10));
        let last = InternalEvent::Event(Event::Resize(20, 20));
        let marker = InternalEvent::PrimaryDeviceAttributes;
        let mut reader = InternalEventReader {
            skipped_events: Vec::new(),
            events: VecDeque::from([first.clone(), marker.clone()]),
            source: Some(Box::new(FakeSource::with_events(&[
                InternalEvent::ColorSchemeReporting(2),
                last.clone(),
                marker,
            ]))),
        };
        let timeout = super::PollTimeout::new(Some(Duration::from_secs(1)));
        assert!(reader.prepare_color_handoff(&timeout).unwrap());
        assert_eq!(reader.events, VecDeque::from([first, last]));

        // A late DA1 or a previous retry cannot answer the next numbered query.
        reader
            .events
            .push_back(InternalEvent::PrimaryDeviceAttributes);
        reader.discard_buffered_input().unwrap();
        let retry = InternalEvent::ColorHandoffMarker(30001);
        let key = InternalEvent::Event(Event::Key(crate::event::KeyCode::Char('x').into()));
        reader.source = Some(Box::new(FakeSource::with_events(&[
            InternalEvent::PrimaryDeviceAttributes,
            InternalEvent::ColorHandoffMarker(30000),
            key.clone(),
            retry.clone(),
        ])));
        assert!(reader.poll(timeout.leftover(), &retry).unwrap());
        assert_eq!(reader.try_read(&retry), Some(retry));
        reader.finish_color_handoff();
        assert_eq!(reader.events, VecDeque::from([key]));
    }

    #[test]
    #[cfg(unix)]
    fn test_discard_buffered_input_clears_all_event_queues() {
        let mut reader = InternalEventReader {
            events: VecDeque::from([InternalEvent::Event(Event::Resize(10, 10))]),
            source: Some(Box::new(FakeSource::with_events(&[InternalEvent::Event(
                Event::Resize(20, 20),
            )]))),
            skipped_events: vec![InternalEvent::CursorPosition(4, 8)],
        };

        assert_eq!(
            reader.discard_buffered_input().unwrap(),
            super::InputDiscardStatus::Complete
        );

        assert!(reader.events.is_empty());
        assert!(reader.skipped_events.is_empty());
        assert_eq!(
            reader
                .source
                .as_mut()
                .unwrap()
                .try_read(Some(Duration::from_secs(0)))
                .unwrap(),
            None
        );
    }

    #[test]
    fn test_poll_timeouts_if_source_has_no_events() {
        let source = FakeSource::default();

        let mut reader = InternalEventReader {
            events: VecDeque::new(),
            source: Some(Box::new(source)),
            skipped_events: Vec::with_capacity(32),
        };

        assert!(
            !reader
                .poll(Some(Duration::from_secs(0)), &InternalEventFilter)
                .unwrap()
        );
    }

    #[test]
    fn test_poll_returns_true_if_source_has_at_least_one_event() {
        let source = FakeSource::with_events(&[InternalEvent::Event(Event::Resize(10, 10))]);

        let mut reader = InternalEventReader {
            events: VecDeque::new(),
            source: Some(Box::new(source)),
            skipped_events: Vec::with_capacity(32),
        };

        assert!(reader.poll(None, &InternalEventFilter).unwrap());
        assert!(
            reader
                .poll(Some(Duration::from_secs(0)), &InternalEventFilter)
                .unwrap()
        );
    }

    #[test]
    fn test_reads_returns_event_if_source_has_at_least_one_event() {
        const EVENT: InternalEvent = InternalEvent::Event(Event::Resize(10, 10));

        let source = FakeSource::with_events(&[EVENT]);

        let mut reader = InternalEventReader {
            events: VecDeque::new(),
            source: Some(Box::new(source)),
            skipped_events: Vec::with_capacity(32),
        };

        assert_eq!(reader.read(&InternalEventFilter).unwrap(), EVENT);
    }

    #[test]
    fn test_read_returns_events_if_source_has_events() {
        const EVENT: InternalEvent = InternalEvent::Event(Event::Resize(10, 10));

        let source = FakeSource::with_events(&[EVENT, EVENT, EVENT]);

        let mut reader = InternalEventReader {
            events: VecDeque::new(),
            source: Some(Box::new(source)),
            skipped_events: Vec::with_capacity(32),
        };

        assert_eq!(reader.read(&InternalEventFilter).unwrap(), EVENT);
        assert_eq!(reader.read(&InternalEventFilter).unwrap(), EVENT);
        assert_eq!(reader.read(&InternalEventFilter).unwrap(), EVENT);
    }

    #[test]
    fn test_poll_returns_false_after_all_source_events_are_consumed() {
        const EVENT: InternalEvent = InternalEvent::Event(Event::Resize(10, 10));

        let source = FakeSource::with_events(&[EVENT, EVENT, EVENT]);

        let mut reader = InternalEventReader {
            events: VecDeque::new(),
            source: Some(Box::new(source)),
            skipped_events: Vec::with_capacity(32),
        };

        assert_eq!(reader.read(&InternalEventFilter).unwrap(), EVENT);
        assert_eq!(reader.read(&InternalEventFilter).unwrap(), EVENT);
        assert_eq!(reader.read(&InternalEventFilter).unwrap(), EVENT);
        assert!(
            !reader
                .poll(Some(Duration::from_secs(0)), &InternalEventFilter)
                .unwrap()
        );
    }

    #[test]
    fn test_poll_propagates_error() {
        let mut reader = InternalEventReader {
            events: VecDeque::new(),
            source: Some(Box::new(FakeSource::new(&[]))),
            skipped_events: Vec::with_capacity(32),
        };

        assert_eq!(
            reader
                .poll(Some(Duration::from_secs(0)), &InternalEventFilter)
                .err()
                .map(|e| format!("{:?}", e.kind())),
            Some(format!("{:?}", io::ErrorKind::Other))
        );
    }

    #[test]
    fn test_read_propagates_error() {
        let mut reader = InternalEventReader {
            events: VecDeque::new(),
            source: Some(Box::new(FakeSource::new(&[]))),
            skipped_events: Vec::with_capacity(32),
        };

        assert_eq!(
            reader
                .read(&InternalEventFilter)
                .err()
                .map(|e| format!("{:?}", e.kind())),
            Some(format!("{:?}", io::ErrorKind::Other))
        );
    }

    #[test]
    fn test_poll_continues_after_error() {
        const EVENT: InternalEvent = InternalEvent::Event(Event::Resize(10, 10));

        let source = FakeSource::new(&[EVENT, EVENT]);

        let mut reader = InternalEventReader {
            events: VecDeque::new(),
            source: Some(Box::new(source)),
            skipped_events: Vec::with_capacity(32),
        };

        assert_eq!(reader.read(&InternalEventFilter).unwrap(), EVENT);
        assert!(reader.read(&InternalEventFilter).is_err());
        assert!(
            reader
                .poll(Some(Duration::from_secs(0)), &InternalEventFilter)
                .unwrap()
        );
    }

    #[test]
    fn test_read_continues_after_error() {
        const EVENT: InternalEvent = InternalEvent::Event(Event::Resize(10, 10));

        let source = FakeSource::new(&[EVENT, EVENT]);

        let mut reader = InternalEventReader {
            events: VecDeque::new(),
            source: Some(Box::new(source)),
            skipped_events: Vec::with_capacity(32),
        };

        assert_eq!(reader.read(&InternalEventFilter).unwrap(), EVENT);
        assert!(reader.read(&InternalEventFilter).is_err());
        assert_eq!(reader.read(&InternalEventFilter).unwrap(), EVENT);
    }

    #[derive(Default)]
    struct FakeSource {
        events: VecDeque<InternalEvent>,
        error: Option<io::Error>,
    }

    impl FakeSource {
        fn new(events: &[InternalEvent]) -> FakeSource {
            FakeSource {
                events: events.to_vec().into(),
                error: Some(io::Error::other("")),
            }
        }

        fn with_events(events: &[InternalEvent]) -> FakeSource {
            FakeSource {
                events: events.to_vec().into(),
                error: None,
            }
        }
    }

    impl EventSource for FakeSource {
        fn try_read(&mut self, _timeout: Option<Duration>) -> io::Result<Option<InternalEvent>> {
            // Return error if set in case there's just one remaining event
            if self.events.len() == 1 {
                if let Some(error) = self.error.take() {
                    return Err(error);
                }
            }

            // Return all events from the queue
            if let Some(event) = self.events.pop_front() {
                return Ok(Some(event));
            }

            // Return error if there're no more events
            if let Some(error) = self.error.take() {
                return Err(error);
            }

            // Timeout
            Ok(None)
        }

        #[cfg(unix)]
        fn buffer_input(&mut self, _input: &[u8], events: &mut VecDeque<InternalEvent>) {
            events.extend(
                self.events
                    .drain(..)
                    .filter(|event| matches!(event, InternalEvent::Event(_))),
            );
        }

        #[cfg(unix)]
        fn discard_buffered_input(&mut self) -> super::InputDiscardStatus {
            self.events.clear();
            super::InputDiscardStatus::Complete
        }

        #[cfg(all(feature = "event-stream", unix))]
        fn expect_color_replies(&mut self) {}

        #[cfg(all(feature = "event-stream", unix))]
        fn finish_color_handoff(&mut self) {}

        #[cfg(all(feature = "event-stream", unix))]
        fn can_query(&self) -> bool {
            self.events.is_empty()
        }

        #[cfg(all(feature = "event-stream", unix))]
        fn discard_matching(&mut self, matches: fn(&InternalEvent) -> bool) {
            self.events.retain(|event| !matches(event));
        }

        #[cfg(feature = "event-stream")]
        fn waker(&self) -> super::super::sys::Waker {
            unimplemented!();
        }
    }
}
