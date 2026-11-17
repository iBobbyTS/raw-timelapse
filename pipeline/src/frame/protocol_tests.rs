    use super::*;

    use std::io::Cursor;
    fn record(kind: u8, reserved: [u8; 3], length: u64, payload: &[u8]) -> Vec<u8> {
        let mut bytes = b"RAWTLST1".to_vec();
        bytes.push(kind);
        bytes.extend(reserved);
        bytes.extend(length.to_le_bytes());
        bytes.extend(payload);
        bytes
    }
    #[test]
    fn versions_are_selected_by_strategy_not_guessed_from_payload() {
        assert!(Protocol::with_version(Cursor::new(b"RAWTLST1"), true).is_err());
        assert!(Protocol::with_version(Cursor::new(b"RAWTLST2"), false).is_err());
        let mut p = Protocol::with_version(Cursor::new(b"RAWTLST2"), true).unwrap();
        p.end().unwrap();
    }
    #[test]
    fn rejects_wrong_order_reserved_lengths_truncation_and_trailing_data() {
        for (bytes, expected, exact) in [
            (record(3, [0; 3], 2, b"{}"), 1, None),
            (record(1, [0, 1, 0], 2, b"{}"), 1, None),
            (record(4, [0; 3], 12, &[0; 12]), 4, Some(6)),
            (record(4, [0; 3], 6, &[0; 5]), 4, Some(6)),
            (record(1, [0; 3], u64::MAX, b""), 1, None),
        ] {
            assert!(Protocol::new(Cursor::new(bytes))
                .unwrap()
                .record(expected, exact)
                .is_err());
        }
        let mut bytes = record(1, [0; 3], 2, b"{}");
        bytes.push(0);
        let mut parser = Protocol::new(Cursor::new(bytes)).unwrap();
        parser.record(1, None).unwrap();
        assert!(parser.end().is_err());
        let mut parser = Protocol::new(Cursor::new(record(4, [0; 3], 6, &[0; 6]))).unwrap();
        assert_eq!(&*parser.record(4, Some(6)).unwrap(), &[0; 6]);
        parser.end().unwrap();
    }
