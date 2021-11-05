use ffmpeg::format::context::input::Input;
use ffmpeg::util::{channel_layout::ChannelLayout, format::sample::Type, frame::audio::Sample};
use ffmpeg::{
    codec::decoder,
    format::{self, io},
    frame::Audio,
    media,
    packet::{Mut, Packet},
    software::resampling,
};
use ffmpeg_sys::*;
use hound::{SampleFormat, WavSpec, WavWriter};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek};

/// When we update the decoder we either have some samples to process or we need to continue
/// waiting for data to become available
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
enum DecoderState {
    /// Data is ready to be converted
    Ready,
    /// The decoder is still waiting on enough data to extract some samples
    NotReady,
}

fn main() {
    ffmpeg::init().unwrap();

    // Want to make sure it doesn't read whole file in one go
    let callback = BufReader::with_capacity(4096, File::open("sample_44100.wav").unwrap());

    let mut format_context = io::input(callback).unwrap();

    let mut data = vec![];

    let input = format_context.streams().best(media::Type::Audio).unwrap();
    let mut audio_decoder = input.codec().decoder().audio().unwrap();
    match audio_decoder.channels() {
        1 => audio_decoder.set_channel_layout(ChannelLayout::MONO),
        2 => audio_decoder.set_channel_layout(ChannelLayout::STEREO),
        e => panic!("Unsupported channel count: {}", e),
    }

    println!("Input stats");
    println!("* Sample rate: {}Hz", audio_decoder.sample_rate());
    println!("* Channels: {}", audio_decoder.channels());
    println!("* Format: {:?}", audio_decoder.format().name());

    let mut packet = Packet::empty();
    let fmt = format::Sample::I16(Type::Packed);
    println!("Creating resampler");
    let mut resampler = audio_decoder
        .resampler(fmt, ChannelLayout::MONO, 8000)
        .unwrap();

    let mut audio = Audio::empty();
    let mut resampled_audio = Audio::empty();
    println!("Start sample reading");
    'outer: loop {
        match read_samples(
            &mut format_context,
            &mut audio_decoder,
            &mut packet,
            &mut audio,
        ) {
            Ok(DecoderState::NotReady) => continue,
            Ok(DecoderState::Ready) => {
                // go from the data frame to an audio packet. TODO the unwrap :cry:
                resampler.run(&audio, &mut resampled_audio).unwrap();
                data.append(&mut get_samples(&resampled_audio));
                while resampler.delay().is_some() {
                    resampler.flush(&mut resampled_audio).unwrap();
                    data.append(&mut get_samples(&resampled_audio));
                }
            }
            Err(e) if e.as_raw_error() == libc::EOF || e.as_raw_error() >= 0 => {
                // Only negative errors are real errors...
                println!("Decoded last packet: {}", e);
                break;
            }
            Err(e) => {
                // This was to mitigate difference in how the EOF was signalled between two
                // ffmpeg versions
                if e.to_string().to_ascii_lowercase() == "end of file" {
                    break;
                }
                // Actual error!
                panic!("Error transcoding: {}, {}", e, e.as_raw_error());
            }
        }
    }
    println!("Flush decoder and read last bits");
    audio_decoder.flush();

    while let Some(_) = resampler.flush(&mut resampled_audio).unwrap() {
        data.append(&mut get_samples(&resampled_audio));
    }

    let spec = WavSpec {
        channels: 1,
        sample_rate: 8000,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    println!("Write output.wav\n* Sample rate: 8000Hz\n* Channels: 1\n* Format: s16");
    let mut writer = WavWriter::create("output.wav", spec).unwrap();
    for sample in &data {
        writer.write_sample(*sample).unwrap();
    }
    writer.finalize().unwrap();
}

#[inline(always)]
fn read_samples(
    format_context: &mut format::context::Input,
    decoder: &mut decoder::Audio,
    packet: &mut Packet,
    audio: &mut Audio,
) -> Result<DecoderState, ffmpeg::Error> {
    packet.read(format_context)?;
    let packet_res = decoder.send_packet(packet);
    unsafe {
        av_packet_unref(packet.as_mut_ptr());
    }
    packet_res?;
    match decoder.receive_frame(audio) {
        Ok(()) => Ok(DecoderState::Ready),
        Err(e) if e.as_raw_error() == libc::EAGAIN => {
            // The decoder needs to be given more data to decode samples
            Ok(DecoderState::NotReady)
        }
        Err(e) => Err(e),
    }
}

/// Get samples from first channel
#[inline]
pub(crate) fn get_samples(audio: &Audio) -> Vec<i16> {
    audio
        .plane::<i16>(0)
        .iter()
        .step_by(audio.channels() as usize)
        .copied()
        .collect()
}
