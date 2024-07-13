use std::ops::Range;

pub fn skip_over_extensions(packet: &Vec<u8>, payload: Range<usize>) -> Option<Range<usize>> {
    let (start, end) = (payload.start, payload.end);
    let mut original_payload = packet[payload].iter();

    // Not documented anywhere, taken from https://github.com/discord-jda/JDA/blob/ca1da012650c9be33cfef47681a2076767dbc58d/src/main/java/net/dv8tion/jda/internal/audio/AudioPacket.java#L110
    // This is explicitly not rfc8285 even though it may kind of look like it.
    if *original_payload.next()? != 0xBE || *original_payload.next()? != 0xDE {
        return Some(start..end);
    }
    let entries = {
        let hi = *original_payload.next()?;
        let lo = *original_payload.next()?;
        (hi as usize) << 8 | (lo as usize)
    };

    for _ in 0..entries * 4 {
        original_payload.next()?;
    }

    let skipped_bytes = end - start - original_payload.len();
    let start = start + skipped_bytes;

    Some((start)..end)
}

#[cfg(test)]
mod test {
    use super::skip_over_extensions;

    #[test]
    fn skip_over_extensions_valid() {
        let data = hex::decode("BEDE000232DF690410FF9000F8FFFE").unwrap();
        let range = skip_over_extensions(&data, 0..data.len());
        assert_eq!(range, Some(12..data.len()));
    }
}
