use rtp_rs::{RtpReader, RtpReaderError, Seq};
use sdp::SessionDescription;
use std::{iter::Map, slice::Chunks, u16};

pub fn session_id(sdp: &SessionDescription) -> String {
    format!("{} {}", sdp.origin.session_id, sdp.origin.session_version)
}

pub struct RtpIter<'a> {
    consistent: Option<ConsistentRtpIterator<'a>>,
    inconsistent: Option<InconsistentRtpIterator<'a>>,
}

impl<'a> RtpIter<'a> {
    pub fn new(buffer: &'a [u8], packet_len: usize) -> Result<Self, RtpReaderError> {
        if buffer.len() % packet_len != 0 {
            log::warn!(
                "Buffer len {} is not a multiple of packet len {}, this is likely a bug.",
                buffer.len(),
                packet_len
            );
        }

        if is_consistent(&buffer, packet_len)? {
            Ok(Self {
                consistent: Some(ConsistentRtpIter(buffer, packet_len).into_iter()),
                inconsistent: None,
            })
        } else {
            log::debug!("inconsistent packet sequence");
            Ok(Self {
                consistent: None,
                inconsistent: Some(InconsistentRtpIterator::new(buffer, packet_len)?),
            })
        }
    }
}

impl<'a> Iterator for RtpIter<'a> {
    type Item = RtpReader<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(iter) = self.consistent.as_mut() {
            iter.next()
        } else if let Some(iter) = self.inconsistent.as_mut() {
            iter.next()
        } else {
            None
        }
    }
}

type ConsistentRtpIterator<'a> = Map<Chunks<'a, u8>, fn(&[u8]) -> RtpReader>;
struct ConsistentRtpIter<'a>(&'a [u8], usize);

impl<'a> IntoIterator for ConsistentRtpIter<'a> {
    type Item = RtpReader<'a>;

    type IntoIter = ConsistentRtpIterator<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.chunks(self.1).map(to_rtp)
    }
}

fn to_rtp(buf: &[u8]) -> RtpReader {
    RtpReader::new(buf).expect("already checked")
}

struct InconsistentRtpIterator<'a> {
    packet_len: usize,
    seq_min: Seq,
    seq_max: Seq,
    next_seq: Option<Seq>,
    current: usize,
    buffer: &'a [u8],
}

impl<'a> InconsistentRtpIterator<'a> {
    fn new(buffer: &'a [u8], packet_len: usize) -> Result<Self, RtpReaderError> {
        let mut seq_min = Seq::from(u16::MAX);
        let mut seq_max = Seq::from(0);

        let first = RtpReader::new(&buffer[0..packet_len])?;
        let might_wrap =
            u16::from(first.sequence_number()) as usize > u16::MAX as usize - 2 * buffer.len();

        for chunk in buffer.chunks(packet_len) {
            let rtp = RtpReader::new(chunk)?;
            let seq = rtp.sequence_number();

            if seq < seq_min && (!might_wrap || (u16::from(seq) > u16::MAX / 2)) {
                seq_min = seq
            }
            if seq > seq_max && (!might_wrap || (u16::from(seq) < u16::MAX / 2)) {
                seq_max = seq
            }
        }

        let next_seq = Some(seq_min);
        let current = 0;

        // log::debug!("Building inconsistent RTP iterator");
        // dbg!(buffer.len());
        // dbg!(&first.sequence_number());
        // dbg!(&seq_min);
        // dbg!(&seq_max);
        // dbg!(&next_seq);
        // dbg!(&current);
        // dbg!(&might_wrap);

        Ok(InconsistentRtpIterator {
            packet_len,
            seq_min,
            seq_max,
            next_seq,
            current,
            buffer,
        })
    }
}

impl<'a> Iterator for InconsistentRtpIterator<'a> {
    type Item = RtpReader<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let start = self.current;
        self.current = start + self.packet_len;

        if self.current > self.buffer.len() {
            return None;
        }

        let item = RtpReader::new(&self.buffer[start..self.current]).expect("already checked");

        if let Some(next_seq) = self.next_seq {
            if item.sequence_number() == next_seq {
                if self.current == self.buffer.len() {
                    self.next_seq = None;
                } else {
                    self.next_seq = Some(next_seq.next());
                }
                Some(item)
            } else {
                let next = self.next();
                self.current = (self.current as i32 - 2 * self.packet_len as i32).max(0) as usize;
                next
            }
        } else {
            None
        }
    }
}

fn is_consistent(buf: &[u8], packet_len: usize) -> Result<bool, RtpReaderError> {
    let mut last_seq: Option<Seq> = None;

    for chunk in buf.chunks(packet_len) {
        let rtp = RtpReader::new(chunk)?;
        let seq = rtp.sequence_number();
        if let Some(last_seq) = last_seq {
            if !last_seq.precedes(seq) {
                return Ok(false);
            }
        }
        last_seq = Some(seq);
    }

    Ok(true)
}
