use std::{collections::vec_deque::VecDeque, io, time::Duration};

#[cfg(unix)]
use crate::event::source::unix::UnixInternalEventSource;
#[cfg(windows)]
use crate::event::source::windows::WindowsEventSource;
#[cfg(feature = "event-stream")]
use crate::event::sys::Waker;
#[cfg(unix)]
use crate::event::InputDiscardStatus;
use crate::event::{filter::Filter, source::EventSource, timeout::PollTimeout, InternalEvent};

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
        let source = self.source.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::Other, "Failed to initialize input reader")
        })?;
        source.buffer_input(input, &mut self.events);
        Ok(())
    }

    #[cfg(unix)]
    pub(crate) fn discard_buffered_input(&mut self) -> io::Result<InputDiscardStatus> {
        let source = self.source.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::Other, "Failed to initialize input reader")
        })?;
        let status = source.discard_buffered_input();
        self.events.clear();
        self.skipped_events.clear();
        Ok(status)
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
        for event in &self.events {
            if filter.eval(event) {
                return Ok(true);
            }
        }

        let event_source = match self.source.as_mut() {
            Some(source) => source,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Failed to initialize input reader",
                ))
            }
        };

        let poll_timeout = PollTimeout::new(timeout);

        loop {
            let maybe_event = match event_source.try_read(poll_timeout.leftover()) {
                Ok(None) => None,
                Ok(Some(event)) => {
                    if filter.eval(&event) {
                        Some(event)
                    } else {
                        self.skipped_events.push(event);
                        None
                    }
                }
                Err(e) => {
                    if e.kind() == io::ErrorKind::Interrupted {
                        return Ok(false);
                    }

                    return Err(e);
                }
            };

            if poll_timeout.elapsed() || maybe_event.is_some() {
                self.events.extend(self.skipped_events.drain(..));

                if let Some(event) = maybe_event {
                    self.events.push_front(event);
                    return Ok(true);
                }

                return Ok(false);
            }
        }
    }

    pub(crate) fn read<F>(&mut self, filter: &F) -> io::Result<InternalEvent>
    where
        F: Filter,
    {
        self.read_queued(filter).map(unwrap_processed_paste)
    }

    fn read_queued<F>(&mut self, filter: &F) -> io::Result<InternalEvent>
    where
        F: Filter,
    {
        let mut skipped_events = VecDeque::new();

        loop {
            while let Some(event) = self.events.pop_front() {
                if filter.eval(&event) {
                    while let Some(event) = skipped_events.pop_front() {
                        self.events.push_back(event);
                    }

                    return Ok(event);
                } else {
                    // We can not directly write events back to `self.events`.
                    // If we did, we would put our self's into an endless loop
                    // that would enqueue -> dequeue -> enqueue etc.
                    // This happens because `poll` in this function will always return true if there are events in it's.
                    // And because we just put the non-fulfilling event there this is going to be the case.
                    // Instead we can store them into the temporary buffer,
                    // and then when the filter is fulfilled write all events back in order.
                    skipped_events.push_back(event);
                }
            }

            let _ = self.poll(None, filter)?;
        }
    }

    /// Keep input extracted from a paste in the shared queue so replacing a stream cannot
    /// discard the remainder of an event that has already been read from the terminal.
    #[cfg(all(unix, feature = "event-stream", feature = "bracketed-paste"))]
    pub(crate) fn read_with_color_reports<F>(&mut self, filter: &F) -> io::Result<InternalEvent>
    where
        F: Filter,
    {
        let event = self.read_queued(filter)?;
        if let InternalEvent::Event(crate::event::Event::Paste(text)) = event {
            let mut extracted = VecDeque::new();
            crate::event::stream::color::extract_paste_colors(text, &mut extracted);
            let first = extracted
                .pop_front()
                .expect("paste extraction retains a paste");
            while let Some(event) = extracted.pop_back() {
                self.events.push_front(event);
            }
            Ok(unwrap_processed_paste(first))
        } else {
            Ok(unwrap_processed_paste(event))
        }
    }
}

/// Keep the processed-paste marker private to the shared queue.
fn unwrap_processed_paste(event: InternalEvent) -> InternalEvent {
    match event {
        #[cfg(all(unix, feature = "event-stream", feature = "bracketed-paste"))]
        InternalEvent::ProcessedPaste(text) => {
            InternalEvent::Event(crate::event::Event::Paste(text))
        }
        event => event,
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::{collections::VecDeque, time::Duration};

    #[cfg(unix)]
    use super::super::filter::CursorPositionFilter;
    use super::{super::Event, EventSource, Filter, InternalEvent, InternalEventReader};

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
        assert!(reader
            .poll(Some(Duration::from_secs(0)), &InternalEventFilter)
            .is_err());
        assert!(reader
            .poll(Some(Duration::from_secs(10)), &InternalEventFilter)
            .is_err());
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
    #[cfg(all(unix, feature = "event-stream", feature = "bracketed-paste"))]
    fn color_stream_leaves_extracted_paste_in_shared_reader() {
        use crate::event::{filter::EventFilter, KeyCode, OscColorPayload};

        let key = InternalEvent::Event(Event::Key(KeyCode::Char('z').into()));
        let mut reader = InternalEventReader {
            events: VecDeque::from([
                InternalEvent::Event(Event::Paste(
                    "hello \x1b]11;rgb:11/22/33\x07world".to_string(),
                )),
                key.clone(),
            ]),
            source: None,
            skipped_events: Vec::new(),
        };

        assert_eq!(
            reader
                .read_with_color_reports(&InternalEventFilter)
                .unwrap(),
            InternalEvent::OscColor {
                slot: 11,
                payload: OscColorPayload::Rgb {
                    r: 17,
                    g: 34,
                    b: 51
                },
            }
        );
        // A replacement reader, including an ordinary EventStream, must still see the paste.
        assert_eq!(
            reader.read(&EventFilter).unwrap(),
            InternalEvent::Event(Event::Paste("hello world".to_string()))
        );
        assert_eq!(reader.read(&EventFilter).unwrap(), key);
    }

    #[test]
    #[cfg(all(unix, feature = "event-stream", feature = "bracketed-paste"))]
    fn color_stream_does_not_rescan_the_retained_paste() {
        use crate::event::{filter::EventFilter, KeyCode, OscColorPayload};

        for (input, retained) in [
            (
                "before\x1b]11;rgb:11/22/\x1b]10;rgb:aa/bb/cc\x0733\x07after",
                "before\x1b]11;rgb:11/22/33\x07after",
            ),
            (
                "\x1b\x1b]10;rgb:aa/bb/cc\x07]11;rgb:44/55/66\x07plain",
                "\x1b]11;rgb:44/55/66\x07plain",
            ),
        ] {
            let key = InternalEvent::Event(Event::Key(KeyCode::Char('z').into()));
            let mut reader = InternalEventReader {
                events: VecDeque::from([
                    InternalEvent::Event(Event::Paste(input.to_string())),
                    key.clone(),
                ]),
                source: None,
                skipped_events: Vec::new(),
            };
            assert_eq!(
                reader
                    .read_with_color_reports(&InternalEventFilter)
                    .unwrap(),
                InternalEvent::OscColor {
                    slot: 10,
                    payload: OscColorPayload::Rgb {
                        r: 170,
                        g: 187,
                        b: 204
                    },
                }
            );
            assert!(reader.poll(Some(Duration::ZERO), &EventFilter).unwrap());
            assert_eq!(
                reader
                    .read_with_color_reports(&InternalEventFilter)
                    .unwrap(),
                InternalEvent::Event(Event::Paste(retained.to_string()))
            );
            assert_eq!(reader.read(&EventFilter).unwrap(), key);
        }
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

        assert!(!reader
            .poll(Some(Duration::from_secs(0)), &InternalEventFilter)
            .unwrap());
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
        assert!(reader
            .poll(Some(Duration::from_secs(0)), &InternalEventFilter)
            .unwrap());
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
        assert!(!reader
            .poll(Some(Duration::from_secs(0)), &InternalEventFilter)
            .unwrap());
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
                .map(|e| format!("{:?}", &e.kind())),
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
                .map(|e| format!("{:?}", &e.kind())),
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
        assert!(reader
            .poll(Some(Duration::from_secs(0)), &InternalEventFilter)
            .unwrap());
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
                error: Some(io::Error::new(io::ErrorKind::Other, "")),
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

        #[cfg(feature = "event-stream")]
        fn waker(&self) -> super::super::sys::Waker {
            unimplemented!();
        }
    }
}
