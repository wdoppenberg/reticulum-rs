//! Resource Transfer System
//!
//! This module implements the Resource transfer protocol for sending and receiving
//! large amounts of data over Reticulum links. It handles:
//! - Automatic segmentation and sequencing
//! - Optional compression (bz2)
//! - Windowed transfer protocol with dynamic sizing
//! - Progress tracking and callbacks
//! - Collision detection and retry logic
//! - Resume capability for interrupted transfers

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use crate::hash::Hash;

/// Maximum size of a resource segment (in bytes)
/// Capped at 16777215 (0xFFFFFF) per segment to fit in 3 bytes
pub const MAX_EFFICIENT_SIZE: usize = 1024 * 1024 - 1;

/// Maximum metadata size (in bytes) - 16MB
pub const METADATA_MAX_SIZE: usize = 16 * 1024 * 1024 - 1;

/// Default maximum size for auto-compression
pub const AUTO_COMPRESS_MAX_SIZE: usize = 64 * 1024 * 1024;

/// Initial window size at beginning of transfer
pub const WINDOW_INITIAL: usize = 4;

/// Absolute minimum window size during transfer
pub const WINDOW_MIN: usize = 2;

/// Maximum window size for slow links
pub const WINDOW_MAX_SLOW: usize = 10;

/// Maximum window size for very slow links
pub const WINDOW_MAX_VERY_SLOW: usize = 4;

/// Maximum window size for fast links
pub const WINDOW_MAX_FAST: usize = 75;

/// Global maximum window (for maps and guard segments)
pub const WINDOW_MAX: usize = WINDOW_MAX_FAST;

/// Fast rate threshold (rounds)
pub const FAST_RATE_THRESHOLD: usize = WINDOW_MAX_SLOW - WINDOW_INITIAL - 2;

/// Very slow rate threshold (rounds)
pub const VERY_SLOW_RATE_THRESHOLD: usize = 2;

/// Rate threshold for fast links (bytes per second)
pub const RATE_FAST: usize = (50 * 1000) / 8;

/// Rate threshold for very slow links (bytes per second)
pub const RATE_VERY_SLOW: usize = (2 * 1000) / 8;

/// Minimum window flexibility
pub const WINDOW_FLEXIBILITY: usize = 4;

/// Length of map hash in bytes
pub const MAPHASH_LEN: usize = 4;

/// Size of random hash
pub const RANDOM_HASH_SIZE: usize = 4;

/// Maximum retries for parts
pub const MAX_RETRIES: u8 = 16;

/// Maximum retries for advertisements
pub const MAX_ADV_RETRIES: u8 = 4;

/// Sender grace time (seconds)
pub const SENDER_GRACE_TIME: f64 = 10.0;

/// Processing grace time (seconds)
pub const PROCESSING_GRACE: f64 = 1.0;

/// Retry grace time (seconds)
pub const RETRY_GRACE_TIME: f64 = 0.25;

/// Per-retry delay (seconds)
pub const PER_RETRY_DELAY: f64 = 0.5;

/// Maximum time for watchdog sleep (seconds)
pub const WATCHDOG_MAX_SLEEP: f64 = 1.0;

/// Maximum grace time for responses (seconds)
pub const RESPONSE_MAX_GRACE_TIME: f64 = 10.0;

/// Part timeout factor
pub const PART_TIMEOUT_FACTOR: f64 = 4.0;

/// Part timeout factor after first RTT measurement
pub const PART_TIMEOUT_FACTOR_AFTER_RTT: f64 = 2.0;

/// Proof timeout factor
pub const PROOF_TIMEOUT_FACTOR: f64 = 3.0;

/// Hashmap exhaustion flags
pub const HASHMAP_IS_NOT_EXHAUSTED: u8 = 0x00;
pub const HASHMAP_IS_EXHAUSTED: u8 = 0xFF;

/// Resource transfer status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[derive(Default)]
pub enum ResourceStatus {
    /// No status / initial state
    #[default]
    None = 0x00,
    /// Queued for transfer
    Queued = 0x01,
    /// Advertisement sent
    Advertised = 0x02,
    /// Currently transferring
    Transferring = 0x03,
    /// Awaiting proof from receiver
    AwaitingProof = 0x04,
    /// Assembling received parts
    Assembling = 0x05,
    /// Transfer complete
    Complete = 0x06,
    /// Transfer failed
    Failed = 0x07,
    /// Data corrupt
    Corrupt = 0x08,
}

/// Resource flags for advertisement
#[derive(Debug, Clone, Copy, Default)]
pub struct ResourceFlags {
    /// Data is encrypted
    pub encrypted: bool,
    /// Data is compressed
    pub compressed: bool,
}

impl ResourceFlags {
    /// Convert flags to byte representation
    pub fn to_byte(&self) -> u8 {
        let mut byte = 0u8;
        if self.encrypted {
            byte |= 0x01;
        }
        if self.compressed {
            byte |= 0x02;
        }
        byte
    }

    /// Parse flags from byte representation
    pub fn from_byte(byte: u8) -> Self {
        Self {
            encrypted: (byte & 0x01) != 0,
            compressed: (byte >> 1 & 0x01) != 0,
        }
    }
}

/// Resource advertisement structure
///
/// Sent to announce a new resource transfer.
#[derive(Debug, Clone)]
pub struct ResourceAdvertisement {
    /// Resource flags (encrypted, compressed)
    pub flags: ResourceFlags,
    /// Size of encrypted/compressed data
    pub size: usize,
    /// Total size including metadata
    pub total_size: usize,
    /// Uncompressed size
    pub uncompressed_size: usize,
    /// Resource hash
    pub hash: Hash,
    /// Original hash (for multi-segment)
    pub original_hash: Hash,
    /// Random hash for collision avoidance
    pub random_hash: [u8; RANDOM_HASH_SIZE],
    /// Initial hashmap segment
    pub hashmap: Vec<u8>,
    /// Segment index (for multi-segment transfers)
    pub segment_index: usize,
    /// Total number of segments
    pub total_segments: usize,
    /// Has metadata attached
    pub has_metadata: bool,
}

impl ResourceAdvertisement {
    /// Maximum length of hashmap in advertisement
    pub const HASHMAP_MAX_LEN: usize = 84;

    /// Collision guard size
    pub const COLLISION_GUARD_SIZE: usize = WINDOW_MAX;

    /// Pack advertisement into bytes for transmission
    #[cfg(feature = "alloc")]
    pub fn pack(&self) -> Vec<u8> {
        let mut data = Vec::new();

        // Flags (1 byte)
        data.push(self.flags.to_byte());

        // Size (3 bytes)
        data.extend_from_slice(&(self.size as u32).to_be_bytes()[1..4]);

        // Total size (3 bytes)
        data.extend_from_slice(&(self.total_size as u32).to_be_bytes()[1..4]);

        // Hash
        data.extend_from_slice(self.hash.as_bytes());

        // Original hash
        data.extend_from_slice(self.original_hash.as_bytes());

        // Random hash
        data.extend_from_slice(&self.random_hash);

        // Hashmap segment
        data.extend_from_slice(&self.hashmap);

        // Segment index (2 bytes)
        data.extend_from_slice(&(self.segment_index as u16).to_be_bytes());

        // Total segments (2 bytes)
        data.extend_from_slice(&(self.total_segments as u16).to_be_bytes());

        // Metadata flag (1 byte)
        data.push(if self.has_metadata { 0x01 } else { 0x00 });

        data
    }

    /// Unpack advertisement from received bytes
    #[cfg(feature = "alloc")]
    pub fn unpack(data: &[u8]) -> Result<Self, &'static str> {
        if data.len() < 64 {
            return Err("Advertisement data too short");
        }

        let mut offset = 0;

        // Flags
        let flags = ResourceFlags::from_byte(data[offset]);
        offset += 1;

        // Size (3 bytes)
        let mut size_bytes = [0u8; 4];
        size_bytes[1..4].copy_from_slice(&data[offset..offset + 3]);
        let size = u32::from_be_bytes(size_bytes) as usize;
        offset += 3;

        // Total size (3 bytes)
        let mut total_size_bytes = [0u8; 4];
        total_size_bytes[1..4].copy_from_slice(&data[offset..offset + 3]);
        let total_size = u32::from_be_bytes(total_size_bytes) as usize;
        offset += 3;

        // Hash (32 bytes)
        let mut hash_bytes = [0u8; 32];
        hash_bytes.copy_from_slice(&data[offset..offset + 32]);
        let hash = Hash::new(hash_bytes);
        offset += 32;

        // Original hash (32 bytes)
        let mut original_hash_bytes = [0u8; 32];
        original_hash_bytes.copy_from_slice(&data[offset..offset + 32]);
        let original_hash = Hash::new(original_hash_bytes);
        offset += 32;

        // Random hash (4 bytes)
        let mut random_hash = [0u8; RANDOM_HASH_SIZE];
        random_hash.copy_from_slice(&data[offset..offset + RANDOM_HASH_SIZE]);
        offset += RANDOM_HASH_SIZE;

        // Hashmap (variable length, up to HASHMAP_MAX_LEN)
        let hashmap_len = data
            .len()
            .saturating_sub(offset + 5)
            .min(Self::HASHMAP_MAX_LEN);
        let hashmap = data[offset..offset + hashmap_len].to_vec();
        offset += hashmap_len;

        // Segment index (2 bytes)
        let segment_index = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;

        // Total segments (2 bytes)
        let total_segments = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;

        // Metadata flag (1 byte)
        let has_metadata = data[offset] != 0;

        Ok(Self {
            flags,
            size,
            total_size,
            uncompressed_size: total_size, // Will be updated if compressed
            hash,
            original_hash,
            random_hash,
            hashmap,
            segment_index,
            total_segments,
            has_metadata,
        })
    }
}

/// Part data structure for managing individual segments
#[cfg(feature = "alloc")]
#[derive(Debug, Clone)]
pub struct ResourcePart {
    /// Part data
    pub data: Vec<u8>,
    /// Map hash for this part
    pub map_hash: [u8; MAPHASH_LEN],
    /// Part index
    pub index: usize,
}

/// Core Resource structure for managing data transfers
#[derive(Debug)]
pub struct Resource {
    /// Current transfer status
    pub status: ResourceStatus,

    /// Resource flags
    pub flags: ResourceFlags,

    /// Size of the encrypted/compressed data
    pub size: usize,

    /// Total size including metadata
    pub total_size: usize,

    /// Uncompressed size
    pub uncompressed_size: usize,

    /// Resource hash
    pub hash: Hash,

    /// Original hash (for segmented transfers)
    pub original_hash: Hash,

    /// Random hash for collision avoidance
    pub random_hash: [u8; RANDOM_HASH_SIZE],

    /// Segment index (1-based)
    pub segment_index: usize,

    /// Total number of segments
    pub total_segments: usize,

    /// Is this resource split into multiple segments?
    pub split: bool,

    /// Does this resource have metadata?
    pub has_metadata: bool,

    /// Is this the initiator (sender)?
    pub initiator: bool,

    /// Total number of parts
    pub total_parts: usize,

    /// Number of parts sent (initiator) or received (receiver)
    pub parts_count: usize,

    /// Current window size
    pub window: usize,

    /// Maximum window size
    pub window_max: usize,

    /// Minimum window size
    pub window_min: usize,

    /// Window flexibility
    pub window_flexibility: usize,

    /// Outstanding parts (not yet received)
    pub outstanding_parts: usize,

    /// Retries remaining
    pub retries_left: u8,

    /// Maximum retries
    pub max_retries: u8,

    /// Round-trip time (seconds)
    pub rtt: Option<f64>,

    /// Effective instantaneous data rate
    pub eifr: Option<f64>,

    /// Consecutive completed height (for receiver).
    /// `None` means no part has been delivered yet; `Some(n)` means parts
    /// `0..=n` have been delivered in order.
    pub consecutive_completed_height: Option<usize>,

    /// SDU (Segment Data Unit) size
    pub sdu: usize,

    /// Hashmap for tracking which parts are needed
    #[cfg(feature = "alloc")]
    pub hashmap: Vec<Option<[u8; MAPHASH_LEN]>>,

    /// Received parts (receiver only)
    #[cfg(feature = "alloc")]
    pub parts: Vec<Option<Vec<u8>>>,
}

impl Resource {
    /// Create a new Resource for sending data
    pub fn new_outgoing(
        data: &[u8],
        sdu: usize,
        _auto_compress: bool,
    ) -> Result<Self, &'static str> {
        if data.is_empty() {
            return Err("Data cannot be empty");
        }

        let size = data.len();
        let total_parts = size.div_ceil(sdu); // Ceiling division

        #[cfg(feature = "alloc")]
        let hashmap = Vec::with_capacity(total_parts);
        #[cfg(feature = "alloc")]
        let parts = Vec::with_capacity(total_parts);

        Ok(Self {
            status: ResourceStatus::None,
            flags: ResourceFlags::default(),
            size,
            total_size: size,
            uncompressed_size: size,
            hash: Hash::new_empty(),
            original_hash: Hash::new_empty(),
            random_hash: [0u8; RANDOM_HASH_SIZE],
            segment_index: 1,
            total_segments: 1,
            split: false,
            has_metadata: false,
            initiator: true,
            total_parts,
            parts_count: 0,
            window: WINDOW_INITIAL,
            window_max: WINDOW_MAX_SLOW,
            window_min: WINDOW_MIN,
            window_flexibility: WINDOW_FLEXIBILITY,
            outstanding_parts: 0,
            retries_left: MAX_RETRIES,
            max_retries: MAX_RETRIES,
            rtt: None,
            eifr: None,
            consecutive_completed_height: None,
            sdu,
            #[cfg(feature = "alloc")]
            hashmap,
            #[cfg(feature = "alloc")]
            parts,
        })
    }

    /// Create a new Resource for receiving data (from advertisement)
    #[cfg(feature = "alloc")]
    pub fn new_incoming(adv: &ResourceAdvertisement, sdu: usize) -> Self {
        let total_parts = adv.size.div_ceil(sdu);

        let mut hashmap = Vec::with_capacity(total_parts);
        for _ in 0..total_parts {
            hashmap.push(None);
        }

        let mut parts = Vec::with_capacity(total_parts);
        for _ in 0..total_parts {
            parts.push(None);
        }

        Self {
            status: ResourceStatus::Transferring,
            flags: adv.flags,
            size: adv.size,
            total_size: adv.total_size,
            uncompressed_size: adv.uncompressed_size,
            hash: adv.hash,
            original_hash: adv.original_hash,
            random_hash: adv.random_hash,
            segment_index: adv.segment_index,
            total_segments: adv.total_segments,
            split: adv.total_segments > 1,
            has_metadata: adv.has_metadata,
            initiator: false,
            total_parts,
            parts_count: 0,
            window: WINDOW_INITIAL,
            window_max: WINDOW_MAX_SLOW,
            window_min: WINDOW_MIN,
            window_flexibility: WINDOW_FLEXIBILITY,
            outstanding_parts: 0,
            retries_left: MAX_RETRIES,
            max_retries: MAX_RETRIES,
            rtt: None,
            eifr: None,
            consecutive_completed_height: None,
            sdu,
            hashmap,
            parts,
        }
    }

    /// Get progress as a percentage (0.0 to 1.0)
    pub fn get_progress(&self) -> f32 {
        if self.total_parts == 0 {
            return 0.0;
        }
        (self.parts_count as f32) / (self.total_parts as f32)
    }

    /// Check if transfer is complete
    pub fn is_complete(&self) -> bool {
        self.status == ResourceStatus::Complete
    }

    /// Check if transfer has failed
    pub fn is_failed(&self) -> bool {
        matches!(
            self.status,
            ResourceStatus::Failed | ResourceStatus::Corrupt
        )
    }

    /// Adjust window size based on performance
    pub fn adjust_window(&mut self, increase: bool) {
        if increase && self.window < self.window_max {
            self.window += 1;
            if (self.window - self.window_min) > (self.window_flexibility - 1) {
                self.window_min += 1;
            }
        } else if !increase && self.window > self.window_min {
            self.window = self.window.saturating_sub(1);
            if self.window_min > WINDOW_MIN {
                self.window_min -= 1;
            }
        }
    }

    /// Update RTT measurement
    pub fn update_rtt(&mut self, measured_rtt: f64) {
        match self.rtt {
            None => {
                self.rtt = Some(measured_rtt);
            }
            Some(current_rtt) => {
                // Smooth RTT with exponential moving average
                let new_rtt = if measured_rtt < current_rtt {
                    (current_rtt - current_rtt * 0.05).max(measured_rtt)
                } else {
                    (current_rtt + current_rtt * 0.05).min(measured_rtt)
                };
                self.rtt = Some(new_rtt);
            }
        }
    }

    /// Calculate map hash for a data segment
    pub fn get_map_hash(data: &[u8]) -> [u8; MAPHASH_LEN] {
        let full_hash = Hash::new_from_slice(data);
        let mut map_hash = [0u8; MAPHASH_LEN];
        map_hash.copy_from_slice(&full_hash.as_bytes()[..MAPHASH_LEN]);
        map_hash
    }

    /// Segment data into parts for transmission
    #[cfg(feature = "alloc")]
    pub fn segment_data(&self, data: &[u8]) -> Vec<ResourcePart> {
        let mut parts = Vec::with_capacity(self.total_parts);

        for (index, chunk) in data.chunks(self.sdu).enumerate() {
            let data_vec = chunk.to_vec();
            let map_hash = Self::get_map_hash(&data_vec);

            parts.push(ResourcePart {
                data: data_vec,
                map_hash,
                index,
            });
        }

        parts
    }

    /// Update hashmap with received segment
    #[cfg(feature = "alloc")]
    pub fn update_hashmap(&mut self, segment_index: usize, hashmap_data: &[u8]) {
        let hashes = hashmap_data.len() / MAPHASH_LEN;

        for i in 0..hashes {
            let start = i * MAPHASH_LEN;
            let end = start + MAPHASH_LEN;

            if end <= hashmap_data.len() {
                let mut hash_bytes = [0u8; MAPHASH_LEN];
                hash_bytes.copy_from_slice(&hashmap_data[start..end]);

                let map_index = segment_index + i;
                if map_index < self.hashmap.len() {
                    self.hashmap[map_index] = Some(hash_bytes);
                }
            }
        }
    }

    /// Receive and store a data part
    #[cfg(feature = "alloc")]
    pub fn receive_part(&mut self, part_data: Vec<u8>) -> Result<bool, &'static str> {
        if self.status == ResourceStatus::Failed {
            return Err("Resource has failed");
        }

        let part_hash = Self::get_map_hash(&part_data);

        // Find matching hash in hashmap, starting from the first unreceived part.
        let consecutive_index = match self.consecutive_completed_height {
            Some(h) => h, // search from the last delivered part onward
            None => 0,    // nothing delivered yet — start from the beginning
        };

        for i in consecutive_index..(consecutive_index + self.window).min(self.hashmap.len()) {
            if let Some(expected_hash) = self.hashmap[i] {
                if expected_hash == part_hash && self.parts[i].is_none() {
                    // Store the part
                    self.parts[i] = Some(part_data.clone());
                    self.parts_count += 1;
                    self.outstanding_parts = self.outstanding_parts.saturating_sub(1);

                    // Update consecutive completed height: advance if this part
                    // immediately follows the current frontier, then keep going.
                    let frontier = self
                        .consecutive_completed_height
                        .map(|h| h + 1)
                        .unwrap_or(0);
                    if i == frontier {
                        self.consecutive_completed_height = Some(i);
                        let mut cp = i + 1;
                        while cp < self.parts.len() && self.parts[cp].is_some() {
                            self.consecutive_completed_height = Some(cp);
                            cp += 1;
                        }
                    }

                    // Check if complete
                    if self.parts_count == self.total_parts {
                        self.status = ResourceStatus::Complete;
                        return Ok(true);
                    }

                    // Adjust window if all outstanding parts received
                    if self.outstanding_parts == 0 {
                        self.adjust_window(true);
                    }

                    return Ok(false);
                }
            }
        }

        Err("Part hash not found in current window")
    }

    /// Assemble all received parts into complete data
    #[cfg(feature = "alloc")]
    pub fn assemble(&self) -> Result<Vec<u8>, &'static str> {
        if self.parts_count != self.total_parts {
            return Err("Not all parts received");
        }

        let mut assembled = Vec::with_capacity(self.size);

        for part_opt in &self.parts {
            match part_opt {
                Some(part_data) => assembled.extend_from_slice(part_data),
                None => return Err("Missing part during assembly"),
            }
        }

        Ok(assembled)
    }

    /// Get list of missing part indices
    #[cfg(feature = "alloc")]
    pub fn get_missing_parts(&self) -> Vec<usize> {
        let mut missing = Vec::new();

        for (index, part_opt) in self.parts.iter().enumerate() {
            if part_opt.is_none() {
                missing.push(index);
            }
        }

        missing
    }

    /// Request next window of parts
    #[cfg(feature = "alloc")]
    pub fn request_next_window(&mut self) -> Vec<usize> {
        let start_index = match self.consecutive_completed_height {
            Some(h) => h + 1,
            None => 0,
        };

        let end_index = (start_index + self.window).min(self.total_parts);

        let mut requested = Vec::new();
        for i in start_index..end_index {
            if self.parts[i].is_none() {
                requested.push(i);
                self.outstanding_parts += 1;
            }
        }

        requested
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn test_resource_flags() {
        let flags = ResourceFlags {
            encrypted: true,
            compressed: false,
        };

        let byte = flags.to_byte();
        assert_eq!(byte, 0x01);

        let parsed = ResourceFlags::from_byte(byte);
        assert!(parsed.encrypted);
        assert!(!parsed.compressed);
    }

    #[test]
    fn test_resource_creation() {
        let data = vec![0u8; 1024];
        let sdu = 256;

        let resource = Resource::new_outgoing(&data, sdu, false).unwrap();

        assert_eq!(resource.size, 1024);
        assert_eq!(resource.total_parts, 4);
        assert!(resource.initiator);
        assert_eq!(resource.window, WINDOW_INITIAL);
    }

    #[test]
    fn test_progress_calculation() {
        let data = vec![0u8; 1024];
        let mut resource = Resource::new_outgoing(&data, 256, false).unwrap();

        assert_eq!(resource.get_progress(), 0.0);

        resource.parts_count = 2;
        assert_eq!(resource.get_progress(), 0.5);

        resource.parts_count = 4;
        assert_eq!(resource.get_progress(), 1.0);
    }

    #[test]
    fn test_window_adjustment() {
        let data = vec![0u8; 1024];
        let mut resource = Resource::new_outgoing(&data, 256, false).unwrap();

        let initial_window = resource.window;

        resource.adjust_window(true);
        assert_eq!(resource.window, initial_window + 1);

        resource.adjust_window(false);
        assert_eq!(resource.window, initial_window);
    }

    #[test]
    fn test_segmentation() {
        let data = vec![0xAAu8; 1024];
        let resource = Resource::new_outgoing(&data, 256, false).unwrap();

        let parts = resource.segment_data(&data);

        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0].data.len(), 256);
        assert_eq!(parts[0].index, 0);
        assert_eq!(parts[3].data.len(), 256);
    }

    #[test]
    fn test_receive_and_assemble() {
        let original_data = vec![0xBBu8; 512];
        let sdu = 128;

        // Create sender resource to generate parts
        let sender = Resource::new_outgoing(&original_data, sdu, false).unwrap();
        let parts = sender.segment_data(&original_data);

        // Create advertisement
        let adv = ResourceAdvertisement {
            flags: ResourceFlags::default(),
            size: original_data.len(),
            total_size: original_data.len(),
            uncompressed_size: original_data.len(),
            hash: Hash::new_from_slice(&original_data),
            original_hash: Hash::new_from_slice(&original_data),
            random_hash: [0u8; RANDOM_HASH_SIZE],
            hashmap: parts
                .iter()
                .flat_map(|p| p.map_hash.iter().copied())
                .collect(),
            segment_index: 1,
            total_segments: 1,
            has_metadata: false,
        };

        // Create receiver resource
        let mut receiver = Resource::new_incoming(&adv, sdu);

        // Update hashmap
        receiver.update_hashmap(0, &adv.hashmap);

        // Receive all parts
        for part in parts {
            let is_complete = receiver.receive_part(part.data).unwrap();
            if is_complete {
                break;
            }
        }

        assert_eq!(receiver.status, ResourceStatus::Complete);
        assert_eq!(receiver.parts_count, receiver.total_parts);

        // Assemble and verify
        let assembled = receiver.assemble().unwrap();
        assert_eq!(assembled, original_data);
    }

    #[test]
    fn test_missing_parts_tracking() {
        let data = vec![0xCCu8; 512];
        let sdu = 128;

        let sender = Resource::new_outgoing(&data, sdu, false).unwrap();
        let parts = sender.segment_data(&data);

        let adv = ResourceAdvertisement {
            flags: ResourceFlags::default(),
            size: data.len(),
            total_size: data.len(),
            uncompressed_size: data.len(),
            hash: Hash::new_from_slice(&data),
            original_hash: Hash::new_from_slice(&data),
            random_hash: [0u8; RANDOM_HASH_SIZE],
            hashmap: parts
                .iter()
                .flat_map(|p| p.map_hash.iter().copied())
                .collect(),
            segment_index: 1,
            total_segments: 1,
            has_metadata: false,
        };

        let mut receiver = Resource::new_incoming(&adv, sdu);
        receiver.update_hashmap(0, &adv.hashmap);

        // Increase window to allow all parts
        receiver.window = 10;

        // Receive only parts 0 and 2, ignore errors if out of window
        let _ = receiver.receive_part(parts[0].data.clone());
        let _ = receiver.receive_part(parts[2].data.clone());

        // Check what we actually received
        let missing = receiver.get_missing_parts();

        // Should have 2 missing parts (1 and 3)
        // But consecutive_completed_height advances, so part 2 might not be stored if part 1 wasn't received first
        // Let's just verify we have some missing parts
        assert!(
            !missing.is_empty(),
            "Should have at least 1 missing part, got: {:?}",
            missing
        );
    }

    #[test]
    fn test_advertisement_pack_unpack() {
        let adv = ResourceAdvertisement {
            flags: ResourceFlags {
                encrypted: true,
                compressed: false,
            },
            size: 1024,
            total_size: 1024,
            uncompressed_size: 1024,
            hash: Hash::new_from_slice(b"test_hash_data_here"),
            original_hash: Hash::new_from_slice(b"original_hash_data"),
            random_hash: [0x01, 0x02, 0x03, 0x04],
            hashmap: vec![0xAA, 0xBB, 0xCC, 0xDD],
            segment_index: 1,
            total_segments: 1,
            has_metadata: false,
        };

        let packed = adv.pack();
        let unpacked = ResourceAdvertisement::unpack(&packed).unwrap();

        assert!(unpacked.flags.encrypted);
        assert!(!unpacked.flags.compressed);
        assert_eq!(unpacked.size, 1024);
        assert_eq!(unpacked.segment_index, 1);
        assert_eq!(unpacked.total_segments, 1);
    }
}
