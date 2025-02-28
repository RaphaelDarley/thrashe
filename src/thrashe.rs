use std::{
    marker::PhantomData,
    ops::Deref,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
    vec,
};

use crate::provider::{CacheProvider, GlobalCache};

#[derive(Debug, Clone)]
pub struct CacheSpec {
    block_size_bits: u8,
    set_num_bits: u8,
    lines_per_set_bits: u8,
}

impl CacheSpec {
    fn set_num(&self) -> usize {
        1 << self.set_num_bits
    }

    fn lines_per_set(&self) -> usize {
        1 << self.lines_per_set_bits
    }

    fn block_size(&self) -> usize {
        1 << self.block_size_bits
    }

    fn size(&self) -> u64 {
        self.block_size() as u64 * self.set_num() as u64 * self.lines_per_set() as u64
    }

    fn split(&self, address: u64) -> (u32, u32) {
        let set_index = ((address >> self.block_size_bits) & (self.block_size() as u64 - 1)) as u32;
        // this will lose some information with 64 bit addresses, though usually only 40 something bits are used
        let tag = (address >> (self.block_size_bits + self.set_num_bits)) as u32;
        (set_index, tag)
    }
}

impl CacheSpec {
    pub fn spec_8kib_32bit_2way() -> CacheSpec {
        CacheSpec {
            block_size_bits: 5,
            set_num_bits: 7,
            lines_per_set_bits: 1,
        }
    }
}

// TODO: remove valid bit, only hit by null pointers - not allowed
// zeroed so will have the lowest access so will be replaced first anyway
/// 63 - 32 | 31 - 1 | 0
/// tag     | access | valid
struct CacheLineCompact(AtomicU64);

#[derive(Debug, PartialEq, Clone)]
struct CacheLine {
    tag: u32,
    access: u32,
    valid: bool,
}

impl CacheLineCompact {
    pub fn new() -> CacheLineCompact {
        CacheLineCompact(AtomicU64::new(0))
    }

    pub fn fetch_unpack(&self) -> CacheLine {
        let val = self.0.load(Ordering::Relaxed);
        CacheLineCompact::unpack(val)
    }

    fn unpack(val: u64) -> CacheLine {
        let valid = (val & 1) == 1;
        let access = (val as u32) >> 1;
        let tag = (val >> 32) as u32;
        CacheLine { tag, access, valid }
    }

    /// if matches returns Ok(()) else returns the epoch of that line if its valid or None if invalid
    pub fn touch_if_matches(
        &self,
        cand_tag: u32,
        epoch_counter: &AtomicU32,
    ) -> Result<(), Option<u32>> {
        let val = self.0.load(Ordering::Relaxed);
        let line = Self::unpack(val);
        if line.valid && cand_tag == line.tag {
            let epoch = epoch_counter.fetch_add(1, Ordering::Relaxed) << 1;
            let mask: u64 = 0xfffffffe;
            let new_val = (val & !mask) | epoch as u64;
            self.0.store(new_val, Ordering::Relaxed);
            Ok(())
        } else if line.valid {
            Err(Some(line.access))
        } else {
            Err(None)
        }
    }

    pub fn pack_store(&self, value: CacheLine) {
        let mut encoding = value.tag as u64;
        encoding <<= 31;
        encoding |= value.access as u64;
        encoding <<= 1;
        encoding |= value.valid as u64;
        self.0.store(encoding, Ordering::Relaxed);
    }
}

impl Clone for CacheLineCompact {
    fn clone(&self) -> Self {
        Self(AtomicU64::new(self.0.load(Ordering::Relaxed)))
    }
}

pub struct CacheState {
    sets: Vec<Vec<CacheLineCompact>>,
    epoch: AtomicU32,
    spec: CacheSpec,
    hits: AtomicU32,
    misses: AtomicU32,
}

impl CacheState {
    pub fn from_spec(spec: CacheSpec) -> CacheState {
        CacheState {
            sets: vec![vec![CacheLineCompact::new(); spec.lines_per_set()]; spec.set_num()],
            epoch: AtomicU32::new(0),
            spec,
            hits: AtomicU32::new(0),
            misses: AtomicU32::new(0),
        }
    }

    pub fn touch_address(&self, address: u64) {
        let (set_index, tag) = self.spec.split(address);
        let set = &self.sets[set_index as usize];

        let mut oldest = &set[0];
        let mut oldest_epoch = Some(oldest.fetch_unpack().access);

        for line in set.iter() {
            match line.touch_if_matches(tag, &self.epoch) {
                // found entry it has been touched, our work is done
                Ok(_) => {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                // Err(e) if oldest_epoch > e => {
                //     oldest_epoch = e;
                //     oldest = line
                // }
                // _ => {}
                Err(e) => match (oldest_epoch, e) {
                    (None, _) => {}
                    (Some(_), None) => {
                        oldest = line;
                        oldest_epoch = None
                    }
                    (Some(acc_e), Some(cand_e)) => {
                        if cand_e < acc_e {
                            oldest = line;
                            oldest_epoch = None
                        }
                    }
                },
            }
        }

        let epoch = self.epoch.fetch_add(1, Ordering::Relaxed) << 1;
        oldest.pack_store(CacheLine {
            tag,
            access: epoch,
            valid: true,
        });
        self.misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn make_report(&self) -> ThrasheReport {
        let access_count = self.epoch.load(Ordering::Relaxed);
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        ThrasheReport {
            access_count,
            hits,
            misses,
            spec: self.spec.clone(),
        }
    }
}

#[derive(Debug)]
pub struct ThrasheReport {
    access_count: u32,
    hits: u32,
    misses: u32,
    spec: CacheSpec,
}

/// Wrapper type that records dereferences in a cache emulation
pub struct Thrashe<T, C: CacheProvider = GlobalCache> {
    inner: T,
    _marker: PhantomData<C>,
}

const _SAME_SIZE: () = assert!(size_of::<usize>() == size_of::<Thrashe<usize>>());

impl<T> Thrashe<T> {
    pub fn new(value: T) -> Self {
        Thrashe {
            inner: value,
            _marker: PhantomData,
        }
    }
}
impl<T, C: CacheProvider> Thrashe<T, C> {
    pub fn prefetch(value: &Self) {
        if let Some(state) = C::get_cache().read().ok().iter().flat_map(|g| &**g).next() {
            let address = (value as *const Self) as usize as u64;
            state.touch_address(address);
        }
    }
}

impl<T, C: CacheProvider> Deref for Thrashe<T, C> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        if let Some(state) = C::get_cache().read().ok().iter().flat_map(|g| &**g).next() {
            let address = (self as *const Self) as usize as u64;
            state.touch_address(address);
        }

        &self.inner
    }
}

trait SyncAssert: Sync {}
impl<T: Sync> SyncAssert for Thrashe<T> {}

trait SendAssert: Send {}
impl<T: Send> SendAssert for Thrashe<T> {}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn basic() {
        GlobalCache::configure(CacheSpec::spec_8kib_32bit_2way());
        let foo = Thrashe::new(42);

        let _ = *foo;
        let _ = *foo;

        let report = GlobalCache::finish().unwrap();
        println!("{:?}", report);
        assert_eq!(report.hits, 1);
        assert_eq!(report.misses, 1);
    }

    #[test]
    fn trashing() {
        let spec = CacheSpec::spec_8kib_32bit_2way();
        let cache = CacheState::from_spec(spec.clone());
        let array_size = 512;
        let element_size = 8;

        let a_base = 4200;
        let b_base = a_base + array_size * element_size;
        let c_base = b_base + array_size * element_size;

        for i in 0..12 {
            let a_addr = a_base + element_size * i;
            let b_addr = b_base + element_size * i;
            let c_addr = c_base + element_size * i;

            cache.touch_address(a_addr);
            cache.touch_address(b_addr);
            cache.touch_address(c_addr);
        }

        let report = cache.make_report();
        assert_eq!(report.access_count, 36);
        assert_eq!(report.spec.size(), 8192);
        assert_eq!(report.hits, 0);
        assert_eq!(report.misses, 36);
    }

    #[test]
    fn linear_access() {
        let spec = CacheSpec::spec_8kib_32bit_2way();
        let cache = CacheState::from_spec(spec.clone());
        let element_size = 8;

        let a_base = 4200;

        for i in 0..128 {
            let a_addr = a_base + element_size * i;
            cache.touch_address(a_addr);
        }

        let report = cache.make_report();
        assert_eq!(report.access_count, 128);
        assert_eq!(report.spec.size(), 8192);
        assert_eq!(report.hits, 96);
        assert_eq!(report.misses, 32);
    }

    #[test]
    fn pack_unpack() {
        let line = CacheLineCompact::new();
        let val = CacheLine {
            tag: 0xABCDEFAB,
            access: 0x0123456,
            valid: true,
        };
        line.pack_store(val.clone());
        let returned = line.fetch_unpack();

        assert_eq!(val, returned)
    }
}
