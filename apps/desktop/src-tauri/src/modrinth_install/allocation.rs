//! Shared exact heap accounting used by target and confirmation capture.
use super::LocalFile;
use std::collections::BTreeMap;
pub fn text(value: &String) -> usize {
    value.capacity()
}
pub fn optional(value: &Option<String>) -> usize {
    value.as_ref().map_or(0, text)
}
pub fn vector<T>(values: &Vec<T>, heap: impl Fn(&T) -> usize) -> usize {
    values.capacity() * std::mem::size_of::<T>() + values.iter().map(heap).sum::<usize>()
}
pub fn map<K, V>(
    values: &BTreeMap<K, V>,
    key: impl Fn(&K) -> usize,
    value: impl Fn(&V) -> usize,
) -> usize {
    // A node holds at most 11 key/value slots and its header/edges. Charging
    // a full node per live entry safely covers sparse leaf/internal nodes.
    values.len() * (11 * std::mem::size_of::<(K, V)>() + 256)
        + values.iter().map(|(k, v)| key(k) + value(v)).sum::<usize>()
}
pub fn local(file: &LocalFile) -> usize {
    text(&file.kind)
        + text(&file.file_name)
        + text(&file.sha512)
        + text(&file.fingerprint)
        + text(&file.resource_fingerprint)
}
