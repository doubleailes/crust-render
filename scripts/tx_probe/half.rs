//! Does the tiff crate decode a tiff:half .tx correctly, and into which variant?
use std::fs::File; use std::io::BufReader;
use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;
fn main() {
    let mut d = Decoder::new(BufReader::new(File::open("fmt_half_tiff.tx").unwrap())).unwrap();
    println!("SampleFormat  = {:?}", d.get_tag_u32_vec(Tag::SampleFormat));
    println!("BitsPerSample = {:?}", d.get_tag_u32_vec(Tag::BitsPerSample));
    println!("colortype     = {:?}", d.colortype().unwrap());
    d.seek_to_image(1).unwrap();
    let (f, label): (Vec<f32>, &str) = match d.read_chunk(0).unwrap() {
        DecodingResult::F16(v) => (v.iter().map(|&x| f32::from(x)).collect(), "F16"),
        DecodingResult::F32(v) => (v, "F32"),
        DecodingResult::U16(v) => (v.iter().map(|&x| x as f32).collect(), "U16"),
        _ => (vec![], "other"),
    };
    println!("variant = {label}, {} samples", f.len());
    println!("first 6 = {:?}", &f[..6]);
    println!("tile sum = {:.5}", f.iter().map(|&x| x as f64).sum::<f64>());
}
