pub fn length_prefixed_to_annexb(out: &mut Vec<u8>, data: &[u8], length: usize) {
    let mut pos = 0;
    while length > 0 && pos + length <= data.len() {
        let n = data[pos..pos + length]
            .iter()
            .fold(0usize, |a, &b| (a << 8) | b as usize);
        pos += length;
        let Some(nal) = data.get(pos..pos + n) else {
            break;
        };
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
        pos += n;
    }
}
