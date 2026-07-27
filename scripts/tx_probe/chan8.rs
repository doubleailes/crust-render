//! Does the tiff crate handle a >4-channel .tx correctly, and via which API?
use std::fs::File;
use std::io::BufReader;
use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;

fn main() {
    let path = "ch8_f32.tx";
    let mut d = Decoder::new(BufReader::new(File::open(path).unwrap())).unwrap();
    println!("SamplesPerPixel tag   = {:?}", d.get_tag_u32(Tag::SamplesPerPixel));
    println!("ExtraSamples tag      = {:?}", d.get_tag_u32_vec(Tag::ExtraSamples));
    println!("BitsPerSample tag     = {:?}", d.get_tag_u32_vec(Tag::BitsPerSample));
    println!("colortype()           = {:?}", d.colortype().unwrap());
    println!("  num_samples()       = {}", d.colortype().unwrap().num_samples());
    println!("chunk_dimensions()    = {:?}", d.chunk_dimensions());
    println!("chunk_buffer_layout   = {:?}", d.image_chunk_buffer_layout(0));

    // read_chunk: what does it give us?
    let r = d.read_chunk(0).unwrap();
    let v = match &r {
        DecodingResult::F32(v) => v,
        _ => panic!(),
    };
    println!("\nread_chunk(0) -> {} f32 ({} per pixel over 64x64)", v.len(), v.len() / 4096);
    println!("  first 16 values: {:?}", &v[..16]);

    // read_chunk_bytes with a buffer sized for all 8 channels
    let mut d = Decoder::new(BufReader::new(File::open(path).unwrap())).unwrap();
    let mut buf = vec![0u8; 64 * 64 * 8 * 4];
    match d.read_chunk_bytes(0, &mut buf) {
        Ok(()) => {
            let f: Vec<f32> = buf
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            println!("read_chunk_bytes(8ch buffer) OK");
            println!("  first 16 values: {:?}", &f[..16]);
            println!("  nonzero tail? value[32760..32768] = {:?}", &f[32760..32768]);
        }
        Err(e) => println!("read_chunk_bytes FAILED: {e}"),
    }
}
