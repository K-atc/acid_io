//! I/O traits and those implementations which rely only on `core`.

use core::{cmp, fmt, mem, slice};

#[cfg(feature = "alloc")]
use alloc::{string::String, vec::Vec};

#[cfg(feature = "alloc")]
use crate::{io_alloc, Lines, Split};
use crate::{Error, ErrorKind, IoSlice, IoSliceMut, Result};

// Read ==========================================================================================

pub(crate) fn default_read_vectored<F>(read: F, bufs: &mut [IoSliceMut<'_>]) -> Result<usize>
where
    F: FnOnce(&mut [u8]) -> Result<usize>,
{
    let buf = bufs
        .iter_mut()
        .find(|b| !b.is_empty())
        .map_or(&mut [][..], |b| &mut **b);
    read(buf)
}

pub(crate) fn default_read_exact<R: Read + ?Sized>(this: &mut R, mut buf: &mut [u8]) -> Result<()> {
    while !buf.is_empty() {
        match this.read(buf) {
            Ok(0) => break,
            Ok(n) => {
                let tmp = buf;
                buf = &mut tmp[n..];
            }
            Err(ref e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    if !buf.is_empty() {
        Err(Error::new_const(
            ErrorKind::UnexpectedEof,
            &"failed to fill whole buffer",
        ))
    } else {
        Ok(())
    }
}

#[allow(missing_docs)]
pub struct Bytes<R> {
    inner: R,
}

impl<R: Read> Iterator for Bytes<R> {
    type Item = Result<u8>;

    fn next(&mut self) -> Option<Result<u8>> {
        let mut byte = 0;
        loop {
            return match self.inner.read(slice::from_mut(&mut byte)) {
                Ok(0) => None,
                Ok(..) => Some(Ok(byte)),
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => Some(Err(e)),
            };
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        Read::size_hint(&self.inner)
    }
}

/// Reader adapter which limits the bytes read from an underlying reader.
///
/// This struct is generally created by calling [`take`] on a reader.
/// Please see the documentation of [`take`] for more details.
///
/// [`take`]: Read::take
#[derive(Debug)]
pub struct Take<T> {
    inner: T,
    limit: u64,
}

impl<T> Take<T> {
    /// Returns the number of bytes that can be read before this instance will
    /// return EOF.
    ///
    /// # Note
    ///
    /// This instance may reach `EOF` after reading fewer bytes than indicated by
    /// this method if the underlying [`Read`] instance reaches EOF.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::prelude::*;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let s = &b"some bytes";
    /// let handle = s.take(5);
    ///
    /// assert_eq!(handle.limit(), 5);
    /// # Ok(())
    /// # }
    /// ```
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Sets the number of bytes that can be read before this instance will
    /// return EOF. This is the same as constructing a new `Take` instance, so
    /// the amount of bytes read and the previous limit value don't matter when
    /// calling this method.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::prelude::*;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let s = &b"some bytes";
    /// let mut handle = s.take(5);
    /// handle.set_limit(10);
    ///
    /// assert_eq!(handle.limit(), 10);
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_limit(&mut self, limit: u64) {
        self.limit = limit;
    }

    /// Consumes the `Take`, returning the wrapped reader.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::prelude::*;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let s = &b"some bytes";
    /// let mut handle = s.take(5);
    /// handle.set_limit(10);
    ///
    /// let inner: &[u8] = handle.into_inner();
    /// # Ok(())
    /// # }
    /// ```
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// Gets a reference to the underlying reader.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::prelude::*;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let s = &b"some bytes";
    /// let mut handle = s.take(5);
    /// handle.set_limit(10);
    ///
    /// let inner: &&[u8] = handle.get_ref();
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_ref(&self) -> &T {
        &self.inner
    }

    /// Gets a mutable reference to the underlying reader.
    ///
    /// Care should be taken to avoid modifying the internal I/O state of the
    /// underlying reader as doing so may corrupt the internal limit of this
    /// `Take`.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::prelude::*;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let s = &b"some bytes";
    /// let mut handle = s.take(5);
    /// handle.set_limit(10);
    ///
    /// let inner: &mut &[u8] = handle.get_mut();
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T: Read> Read for Take<T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        // Don't call into inner reader at all at EOF because it may still block
        if self.limit == 0 {
            return Ok(0);
        }

        let max = cmp::min(buf.len() as u64, self.limit) as usize;
        let n = self.inner.read(&mut buf[..max])?;
        self.limit -= n as u64;
        Ok(n)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let (min, max) = self.inner.size_hint();

        (
            cmp::min(self.limit, min as u64) as usize,
            max.map(|x| cmp::min(x as u64, self.limit) as usize)
                .or_else(|| self.limit.try_into().ok()),
        )
    }
}

impl<T: BufRead> BufRead for Take<T> {
    fn fill_buf(&mut self) -> Result<&[u8]> {
        // Don't call into inner reader at all at EOF because it may still block
        if self.limit == 0 {
            return Ok(&[]);
        }

        let buf = self.inner.fill_buf()?;
        let cap = cmp::min(buf.len() as u64, self.limit) as usize;
        Ok(&buf[..cap])
    }

    fn consume(&mut self, amt: usize) {
        // Don't let callers reset the limit by passing an overlarge value
        let amt = cmp::min(amt as u64, self.limit) as usize;
        self.limit -= amt as u64;
        self.inner.consume(amt);
    }
}

/// Adapter to chain together two readers.
///
/// This struct is generally created by calling [`chain`] on a reader.
/// Please see the documentation of [`chain`] for more details.
///
/// [`chain`]: Read::chain
#[derive(Debug)]
pub struct Chain<T, U> {
    first: T,
    second: U,
    done_first: bool,
}

impl<T, U> Chain<T, U> {
    /// Consumes the `Chain`, returning the wrapped readers.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::prelude::*;
    /// use acid_io::Cursor;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let mut first = &b"Hello, ";
    /// let mut second = Cursor::new("world!");
    ///
    /// let chain = first.chain(second);
    /// let (first, second): (&[u8], Cursor<&str>) = chain.into_inner();
    /// # Ok(())
    /// # }
    /// ```
    pub fn into_inner(self) -> (T, U) {
        (self.first, self.second)
    }

    /// Gets references to the underlying readers in this `Chain`.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::prelude::*;
    /// use acid_io::Cursor;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let mut first = &b"Hello, ";
    /// let mut second = Cursor::new("world!");
    ///
    /// let chain = first.chain(second);
    /// let (first, second): (&&[u8], &Cursor<&str>) = chain.get_ref();
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_ref(&self) -> (&T, &U) {
        (&self.first, &self.second)
    }

    /// Gets mutable references to the underlying readers in this `Chain`.
    ///
    /// Care should be taken to avoid modifying the internal I/O state of the
    /// underlying readers as doing so may corrupt the internal state of this
    /// `Chain`.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::prelude::*;
    /// use acid_io::Cursor;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let mut first = &b"Hello, ";
    /// let mut second = Cursor::new("world!");
    ///
    /// let mut chain = first.chain(second);
    /// let (first, second): (&mut &[u8], &mut Cursor<&str>) = chain.get_mut();
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_mut(&mut self) -> (&mut T, &mut U) {
        (&mut self.first, &mut self.second)
    }
}

impl<T: Read, U: Read> Read for Chain<T, U> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if !self.done_first {
            match self.first.read(buf)? {
                0 if !buf.is_empty() => self.done_first = true,
                n => return Ok(n),
            }
        }
        self.second.read(buf)
    }

    fn read_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> Result<usize> {
        if !self.done_first {
            match self.first.read_vectored(bufs)? {
                0 if bufs.iter().any(|b| !b.is_empty()) => self.done_first = true,
                n => return Ok(n),
            }
        }
        self.second.read_vectored(bufs)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let (min1, max1) = self.first.size_hint();
        let (min2, max2) = self.second.size_hint();

        (
            min1 + min2,
            max1.map(|x| max2.map(|y| x.checked_add(y)).flatten())
                .flatten(),
        )
    }
}

impl<T: BufRead, U: BufRead> BufRead for Chain<T, U> {
    fn fill_buf(&mut self) -> Result<&[u8]> {
        if !self.done_first {
            match self.first.fill_buf()? {
                buf if buf.is_empty() => {
                    self.done_first = true;
                }
                buf => return Ok(buf),
            }
        }
        self.second.fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        if !self.done_first {
            self.first.consume(amt)
        } else {
            self.second.consume(amt)
        }
    }
}

/// The `Read` trait allows for reading bytes from a source.
///
/// Implementors of the `Read` trait are called 'readers'.
///
/// Readers are defined by one required method, [`read()`]. Each call to [`read()`]
/// will attempt to pull bytes from this source into a provided buffer. A
/// number of other methods are implemented in terms of [`read()`], giving
/// implementors a number of ways to read bytes while only needing to implement
/// a single method.
///
/// # Examples
///
/// Read from [`&str`] because [`&[u8]`][prim@slice] implements `Read`:
///
/// ```
/// use acid_io::prelude::*;
///
/// # fn main() -> acid_io::Result<()> {
/// let mut b = "This string will be read".as_bytes();
/// let mut buffer = [0; 10];
///
/// // read up to 10 bytes
/// b.read(&mut buffer)?;
///
/// # Ok(())
/// # }
/// ```
///
/// [`read()`]: Read::read
/// [`&str`]: prim@str
pub trait Read {
    /// Pull some bytes from this source into the specified buffer, returning
    /// how many bytes were read.
    ///
    /// This function does not provide any guarantees about whether it blocks
    /// waiting for data, but if an object needs to block for a read and cannot,
    /// it will typically signal this via an [`Err`] return value.
    ///
    /// If the return value of this method is [`Ok(n)`], then implementations must
    /// guarantee that `0 <= n <= dst.len()`. A nonzero `n` value indicates
    /// that the buffer `dst` has been filled in with `n` bytes of data from this
    /// source. If `n` is `0`, then it can indicate one of two scenarios:
    ///
    /// 1. This reader has reached its "end of file" and will likely no longer
    ///    be able to produce bytes. Note that this does not mean that the
    ///    reader will *always* no longer be able to produce bytes.
    /// 2. The buffer specified was 0 bytes in length.
    ///
    /// It is not an error if the returned value `n` is smaller than the buffer size,
    /// even when the reader is not at the end of the stream yet.
    /// This may happen for example because fewer bytes are actually available right now
    /// (e. g. being close to end-of-file) or because read() was interrupted by a signal.
    ///
    /// As this trait is safe to implement, callers cannot rely on `n <= dst.len()` for safety.
    /// Extra care needs to be taken when `unsafe` functions are used to access the read bytes.
    /// Callers have to ensure that no unchecked out-of-bounds accesses are possible even if
    /// `n > dst.len()`.
    ///
    /// No guarantees are provided about the contents of `dst` when this
    /// function is called, implementations cannot rely on any property of the
    /// contents of `dst` being true. It is recommended that *implementations*
    /// only write data to `dst` instead of reading its contents.
    ///
    /// Correspondingly, however, *callers* of this method must not assume any guarantees
    /// about how the implementation uses `dst`. The trait is safe to implement,
    /// so it is possible that the code that's supposed to write to the buffer might also read
    /// from it. It is your responsibility to make sure that `dst` is initialized
    /// before calling `read`. Calling `read` with an uninitialized `dst` (of the kind one
    /// obtains via [`MaybeUninit<T>`]) is not safe, and can lead to undefined behavior.
    ///
    /// [`MaybeUninit<T>`]: core::mem::MaybeUninit
    ///
    /// # Errors
    ///
    /// If this function encounters any form of I/O or other error, an error
    /// variant will be returned. If an error is returned then it must be
    /// guaranteed that no bytes were read.
    ///
    /// An error of the [`ErrorKind::Interrupted`] kind is non-fatal and the read
    /// operation should be retried if there is nothing else to do.
    ///
    /// # Examples
    ///
    /// [`Ok(n)`]: Ok
    ///
    /// ```
    /// use acid_io::Read;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let src = [1u8, 2, 3, 4, 5, 6, 7, 8];
    /// let mut dst = [0u8; 3];
    ///
    /// let mut r = &src[..];
    ///
    /// // read up to 3 bytes
    /// let n = r.read(&mut dst[..])?;
    ///
    /// assert_eq!(dst, [1, 2, 3]);
    /// # Ok(())
    /// # }
    /// ```
    fn read(&mut self, dst: &mut [u8]) -> Result<usize>;

    /// Like `read`, except that it reads into a slice of buffers.
    ///
    /// Data is copied to fill each buffer in order, with the final buffer
    /// written to possibly being only partially filled. This method must
    /// behave equivalently to a single call to `read` with concatenated
    /// buffers.
    ///
    /// The default implementation calls `read` with either the first nonempty
    /// buffer provided, or an empty one if none exists.
    fn read_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> Result<usize> {
        default_read_vectored(|b| self.read(b), bufs)
    }

    /// Determines if this `Read`er has an efficient `read_vectored`
    /// implementation.
    ///
    /// If a `Read`er does not override the default `read_vectored`
    /// implementation, code using it may want to avoid the method all together
    /// and coalesce writes into a single buffer for higher performance.
    ///
    /// The default implementation returns `false`.
    #[doc(hidden)]
    fn is_read_vectored(&self) -> bool {
        false
    }

    /// Read all bytes until EOF in this source, placing them into `buf`.
    ///
    /// All bytes read from this source will be appended to the specified buffer
    /// `buf`. This function will continuously call [`read()`] to append more data to
    /// `buf` until [`read()`] returns either [`Ok(0)`] or an error of
    /// non-[`ErrorKind::Interrupted`] kind.
    ///
    /// If successful, this function will return the total number of bytes read.
    ///
    /// # Errors
    ///
    /// If this function encounters an error of the kind
    /// [`ErrorKind::Interrupted`] then the error is ignored and the operation
    /// will continue.
    ///
    /// If any other read error is encountered then this function immediately
    /// returns. Any bytes which have already been read will be appended to
    /// `buf`.
    ///
    /// [`read()`]: Read::read
    /// [`Ok(0)`]: core::result::Result::Ok
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::prelude::*;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let data = b"Let's read this entire buffer!";
    ///
    /// let mut src = &data[..];
    /// let mut dst = Vec::new();
    /// src.read_to_end(&mut dst)?;
    ///
    /// assert_eq!(&data[..], &dst);
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "alloc")]
    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        io_alloc::default_read_to_end(self, buf)
    }

    /// Read all bytes until EOF in this source, appending them to `buf`.
    ///
    /// If successful, this function returns the number of bytes which were read
    /// and appended to `buf`.
    ///
    /// # Errors
    ///
    /// If the data in this stream is *not* valid UTF-8 then an error is
    /// returned and `buf` is unchanged.
    ///
    /// See [`read_to_end`] for other error semantics.
    ///
    /// [`read_to_end`]: Read::read_to_end
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::prelude::*;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let mut buffer = String::new();
    ///
    /// let mut valid = &b"Some valid UTF-8"[..];
    /// valid.read_to_string(&mut buffer)?;
    ///
    /// let mut invalid = &b"\xFF\xFF\xFF\xFF"[..];
    /// assert!(invalid.read_to_string(&mut buffer).is_err());
    ///
    /// assert_eq!(buffer, "Some valid UTF-8");
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "alloc")]
    fn read_to_string(&mut self, buf: &mut String) -> Result<usize> {
        io_alloc::default_read_to_string(self, buf)
    }

    /// Read the exact number of bytes required to fill `buf`.
    ///
    /// This function reads as many bytes as necessary to completely fill the
    /// specified buffer `buf`.
    ///
    /// No guarantees are provided about the contents of `buf` when this
    /// function is called; implementations cannot rely on any property of the
    /// contents of `buf` being true. It is recommended that implementations
    /// only write data to `buf` instead of reading its contents. The
    /// documentation on [`read`] has a more detailed explanation on this
    /// subject.
    ///
    /// # Errors
    ///
    /// If this function encounters an error of the kind
    /// [`ErrorKind::Interrupted`] then the error is ignored and the operation
    /// will continue.
    ///
    /// If this function encounters an "end of file" before completely filling
    /// the buffer, it returns an error of the kind [`ErrorKind::UnexpectedEof`].
    /// The contents of `buf` are unspecified in this case.
    ///
    /// If any other read error is encountered then this function immediately
    /// returns. The contents of `buf` are unspecified in this case.
    ///
    /// If this function returns an error, it is unspecified how many bytes it
    /// has read, but it will never read more than would be necessary to
    /// completely fill the buffer.
    ///
    /// # Examples
    ///
    /// [`read`]: Read::read
    ///
    /// ```
    /// use acid_io::Read;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let src = [1u8, 2, 3, 4, 5, 6, 7, 8];
    /// let mut dst = [0u8; 3];
    ///
    /// let mut r = &src[..];
    ///
    /// // read exactly 3 bytes
    /// let n = r.read_exact(&mut dst[..])?;
    ///
    /// assert_eq!(dst, [1, 2, 3]);
    /// # Ok(())
    /// # }
    /// ```
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        default_read_exact(self, buf)
    }

    /// Creates a "by reference" adapter for this instance of `Read`.
    ///
    /// The returned adapter also implements `Read` and will simply borrow this
    /// current reader.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::prelude::*;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let bytes = [1, 1, 2, 3, 5, 8, 13, 21];
    /// let mut r = &bytes[..];
    ///
    /// { // Use the reader by reference.
    ///     let mut dst = [0u8; 4];
    ///     let mut r2 = r.by_ref();
    ///     r2.read(&mut dst)?;
    ///     assert_eq!(dst, [1, 1, 2, 3]);
    /// } // Drop the mutable borrow on the reader.
    ///
    /// let mut dst = [0u8; 4];
    /// r.read(&mut dst)?;
    /// assert_eq!(dst, [5, 8, 13, 21]);
    /// # Ok(())
    /// # }
    /// ```
    fn by_ref(&mut self) -> &mut Self
    where
        Self: Sized,
    {
        self
    }

    /// Transforms this `Read` instance to an [`Iterator`] over its bytes.
    ///
    /// The returned type implements [`Iterator`] where the [`Item`] is
    /// [`Result<u8>`][Result]; The yielded item is [`Ok`] if a byte was
    /// successfully read and [`Err`] otherwise. EOF is mapped to returning
    /// [`None`] from this iterator.
    ///
    /// # Examples
    ///
    /// [`Item`]: Iterator::Item
    /// [Result]: crate::Result
    ///
    /// ```
    /// use acid_io::prelude::*;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let mut it = "Hi!".as_bytes().bytes();
    ///
    /// assert_eq!(it.next().transpose()?, Some(b'H'));
    /// assert_eq!(it.next().transpose()?, Some(b'i'));
    /// assert_eq!(it.next().transpose()?, Some(b'!'));
    /// assert_eq!(it.next().transpose()?, None);
    /// # Ok(())
    /// # }
    /// ```
    fn bytes(self) -> Bytes<Self>
    where
        Self: Sized,
    {
        Bytes { inner: self }
    }

    /// Creates an adapter which will chain this stream with another.
    ///
    /// The returned `Read` instance will first read all bytes from this object
    /// until EOF is encountered. Afterwards the output is equivalent to the
    /// output of `next`.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::prelude::*;
    /// use acid_io::Cursor;
    ///
    /// # #[cfg(not(feature = "alloc"))]
    /// # fn main() -> acid_io::Result<()> { Ok(()) }
    /// # #[cfg(feature = "alloc")]
    /// # fn main() -> acid_io::Result<()> {
    /// let mut first = &b"Hello, ";
    /// let mut second = Cursor::new("world!");
    ///
    /// let mut s = String::new();
    /// first.chain(second).read_to_string(&mut s)?;
    ///
    /// assert_eq!(s, "Hello, world!");
    /// # Ok(())
    /// # }
    /// ```
    fn chain<R: Read>(self, next: R) -> Chain<Self, R>
    where
        Self: Sized,
    {
        Chain {
            first: self,
            second: next,
            done_first: false,
        }
    }

    /// Creates an adapter which will read at most `limit` bytes from it.
    ///
    /// This function returns a new instance of `Read` which will read at most
    /// `limit` bytes, after which it will always return EOF ([`Ok(0)`]). Any
    /// read errors will not count towards the number of bytes read and future
    /// calls to [`read()`] may succeed.
    ///
    /// # Examples

    /// [`Ok(0)`]: Ok
    /// [`read()`]: Read::read
    ///
    /// ```
    /// use acid_io::prelude::*;
    ///
    /// # #[cfg(not(feature = "alloc"))]
    /// # fn main() -> acid_io::Result<()> { Ok(()) }
    /// # #[cfg(feature = "alloc")]
    /// # fn main() -> acid_io::Result<()> {
    /// let mut r = &b"Bytes and more bytes";
    ///
    /// // read at most five bytes
    /// let mut handle = r.take(5);
    ///
    /// let mut buffer = Vec::new();
    /// handle.read_to_end(&mut buffer)?;
    ///
    /// assert_eq!(&buffer, &b"Bytes");
    /// # Ok(())
    /// # }
    /// ```
    fn take(self, limit: u64) -> Take<Self>
    where
        Self: Sized,
    {
        Take { inner: self, limit }
    }

    #[doc(hidden)]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, None)
    }
}

impl<R: Read> Read for &mut R {
    fn read(&mut self, dst: &mut [u8]) -> Result<usize> {
        (*self).read(dst)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        (*self).read_exact(buf)
    }
}

impl Read for &[u8] {
    #[inline]
    fn read(&mut self, dst: &mut [u8]) -> Result<usize> {
        let copy_len = cmp::min(dst.len(), self.len());
        let (src, rem) = self.split_at(copy_len);

        // Avoid invoking memcpy for very small copies.
        if copy_len == 1 {
            dst[0] = src[0];
        } else {
            dst[..copy_len].copy_from_slice(src);
        }

        *self = rem;

        Ok(copy_len)
    }

    #[inline]
    fn read_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> Result<usize> {
        let mut nread = 0;
        for buf in bufs {
            nread += self.read(buf)?;
            if self.is_empty() {
                break;
            }
        }

        Ok(nread)
    }

    #[inline]
    fn is_read_vectored(&self) -> bool {
        true
    }

    #[inline]
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        if buf.len() > self.len() {
            return Err(Error::new_const(
                ErrorKind::UnexpectedEof,
                &"failed to fill whole buffer",
            ));
        }
        let (a, b) = self.split_at(buf.len());

        // First check if the amount of bytes we want to read is small:
        // `copy_from_slice` will generally expand to a call to `memcpy`, and
        // for a single byte the overhead is significant.
        if buf.len() == 1 {
            buf[0] = a[0];
        } else {
            buf.copy_from_slice(a);
        }

        *self = b;
        Ok(())
    }

    #[inline]
    #[cfg(feature = "alloc")]
    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        buf.extend_from_slice(*self);
        let len = self.len();
        *self = &self[len..];
        Ok(len)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.len(), Some(self.len()))
    }
}

// BufRead =======================================================================================

/// A `BufRead` is a type of `Read`er which has an internal buffer, allowing it
/// to perform extra ways of reading.
///
/// # Examples
///
/// In-memory buffers implement `BufRead`:
///
/// ```
/// use acid_io::prelude::*;
///
/// let input = b"Here\nare\nsome\nlines\n";
///
/// # #[cfg(feature = "alloc")]
/// # {
/// for line in input.lines() {
///     println!("{}", line.unwrap());
/// }
/// # }
/// ```
///
/// If you have something that implements [`Read`], you can use the [`BufReader`
/// type][`BufReader`] to turn it into a `BufRead`.
///
/// [`BufReader`]: crate::BufReader
pub trait BufRead {
    /// Returns the contents of the internal buffer, filling it with more data
    /// from the inner reader if it is empty.
    ///
    /// This function is a lower-level call. It needs to be paired with the
    /// [`consume`] method to function properly. When calling this
    /// method, none of the contents will be "read" in the sense that later
    /// calling `read` may return the same contents. As such, [`consume`] must
    /// be called with the number of bytes that are consumed from this buffer to
    /// ensure that the bytes are never returned twice.
    ///
    /// [`consume`]: BufRead::consume
    ///
    /// An empty buffer returned indicates that the stream has reached EOF.
    ///
    /// # Errors
    ///
    /// This function will return an I/O error if the underlying reader was
    /// read, but returned an error.
    fn fill_buf(&mut self) -> Result<&[u8]>;

    /// Tells this buffer that `amt` bytes have been consumed from the buffer,
    /// so they should no longer be returned in calls to `read`.
    ///
    /// This function is a lower-level call. It needs to be paired with the
    /// [`fill_buf`] method to function properly. This function does
    /// not perform any I/O, it simply informs this object that some amount of
    /// its buffer, returned from [`fill_buf`], has been consumed and should
    /// no longer be returned. As such, this function may do odd things if
    /// [`fill_buf`] isn't called before calling it.
    ///
    /// The `amt` must be `<=` the number of bytes in the buffer returned by
    /// [`fill_buf`].
    ///
    /// # Examples
    ///
    /// Since `consume()` is meant to be used with [`fill_buf`],
    /// that method's example includes an example of `consume()`.
    ///
    /// [`fill_buf`]: BufRead::fill_buf
    fn consume(&mut self, amt: usize);

    /// Read all bytes into `buf` until the delimiter `byte` or EOF is reached.
    ///
    /// This function will read bytes from the underlying stream until the
    /// delimiter or EOF is found. Once found, all bytes up to, and including,
    /// the delimiter (if found) will be appended to `buf`.
    ///
    /// If successful, this function will return the total number of bytes read.
    ///
    /// This function is blocking and should be used carefully: it is possible for
    /// an attacker to continuously send bytes without ever sending the delimiter
    /// or EOF.
    ///
    /// # Errors
    ///
    /// This function will ignore all instances of [`ErrorKind::Interrupted`] and
    /// will otherwise return any errors returned by [`fill_buf`].
    ///
    /// If an I/O error is encountered then all bytes read so far will be
    /// present in `buf` and its length will have been adjusted appropriately.
    ///
    /// [`fill_buf`]: BufRead::fill_buf
    ///
    /// # Examples
    ///
    /// [`acid_io::Cursor`][`Cursor`] is a type that implements `BufRead`. In
    /// this example, we use [`Cursor`] to read all the bytes in a byte slice
    /// in hyphen delimited segments:
    ///
    /// ```
    /// use acid_io::BufRead;
    ///
    /// let mut cursor = acid_io::Cursor::new(b"lorem-ipsum");
    /// let mut buf = vec![];
    ///
    /// // cursor is at 'l'
    /// let num_bytes = cursor.read_until(b'-', &mut buf)
    ///     .expect("reading from cursor won't fail");
    /// assert_eq!(num_bytes, 6);
    /// assert_eq!(buf, b"lorem-");
    /// buf.clear();
    ///
    /// // cursor is at 'i'
    /// let num_bytes = cursor.read_until(b'-', &mut buf)
    ///     .expect("reading from cursor won't fail");
    /// assert_eq!(num_bytes, 5);
    /// assert_eq!(buf, b"ipsum");
    /// buf.clear();
    ///
    /// // cursor is at EOF
    /// let num_bytes = cursor.read_until(b'-', &mut buf)
    ///     .expect("reading from cursor won't fail");
    /// assert_eq!(num_bytes, 0);
    /// assert_eq!(buf, b"");
    /// ```
    #[cfg(feature = "alloc")]
    fn read_until(&mut self, byte: u8, buf: &mut Vec<u8>) -> Result<usize> {
        io_alloc::read_until(self, byte, buf)
    }

    /// Read all bytes until a newline (the `0xA` byte) is reached, and append
    /// them to the provided buffer.
    ///
    /// This function will read bytes from the underlying stream until the
    /// newline delimiter (the `0xA` byte) or EOF is found. Once found, all bytes
    /// up to, and including, the delimiter (if found) will be appended to
    /// `buf`.
    ///
    /// If successful, this function will return the total number of bytes read.
    ///
    /// If this function returns [`Ok(0)`], the stream has reached EOF.
    ///
    /// This function is blocking and should be used carefully: it is possible for
    /// an attacker to continuously send bytes without ever sending a newline
    /// or EOF.
    ///
    /// [`Ok(0)`]: Ok
    ///
    /// # Errors
    ///
    /// This function has the same error semantics as [`read_until`] and will
    /// also return an error if the read bytes are not valid UTF-8. If an I/O
    /// error is encountered then `buf` may contain some bytes already read in
    /// the event that all data read so far was valid UTF-8.
    ///
    /// [`read_until`]: BufRead::read_until
    ///
    /// # Examples
    ///
    /// [`acid_io::Cursor`][`Cursor`] is a type that implements `BufRead`. In
    /// this example, we use [`Cursor`] to read all the lines in a byte slice:
    ///
    /// ```
    /// use acid_io::{self, BufRead};
    ///
    /// let mut cursor = acid_io::Cursor::new(b"foo\nbar");
    /// let mut buf = String::new();
    ///
    /// // cursor is at 'f'
    /// let num_bytes = cursor.read_line(&mut buf)
    ///     .expect("reading from cursor won't fail");
    /// assert_eq!(num_bytes, 4);
    /// assert_eq!(buf, "foo\n");
    /// buf.clear();
    ///
    /// // cursor is at 'b'
    /// let num_bytes = cursor.read_line(&mut buf)
    ///     .expect("reading from cursor won't fail");
    /// assert_eq!(num_bytes, 3);
    /// assert_eq!(buf, "bar");
    /// buf.clear();
    ///
    /// // cursor is at EOF
    /// let num_bytes = cursor.read_line(&mut buf)
    ///     .expect("reading from cursor won't fail");
    /// assert_eq!(num_bytes, 0);
    /// assert_eq!(buf, "");
    /// ```
    #[cfg(feature = "alloc")]
    fn read_line(&mut self, buf: &mut String) -> Result<usize> {
        // Note that we are not calling the `.read_until` method here, but
        // rather our hardcoded implementation. For more details as to why, see
        // the comments in `read_to_end`.
        unsafe { io_alloc::append_to_string(buf, |b| io_alloc::read_until(self, b'\n', b)) }
    }

    /// Returns an iterator over the contents of this reader split on the byte
    /// `byte`.
    ///
    /// The iterator returned from this function will return instances of
    /// <code>[io::Result]<[Vec]\<u8>></code>. Each vector returned will *not* have
    /// the delimiter byte at the end.
    ///
    /// This function will yield errors whenever [`read_until`] would have
    /// also yielded an error.
    ///
    /// [io::Result]: self::Result "io::Result"
    /// [`read_until`]: BufRead::read_until
    ///
    /// # Examples
    ///
    /// [`acid_io::Cursor`][`Cursor`] is a type that implements `BufRead`. In
    /// this example, we use [`Cursor`] to iterate over all hyphen delimited
    /// segments in a byte slice
    ///
    /// ```
    /// use acid_io::BufRead;
    ///
    /// let cursor = acid_io::Cursor::new(b"lorem-ipsum-dolor");
    ///
    /// let mut split_iter = cursor.split(b'-').map(|l| l.unwrap());
    /// assert_eq!(split_iter.next(), Some(b"lorem".to_vec()));
    /// assert_eq!(split_iter.next(), Some(b"ipsum".to_vec()));
    /// assert_eq!(split_iter.next(), Some(b"dolor".to_vec()));
    /// assert_eq!(split_iter.next(), None);
    /// ```
    #[cfg(feature = "alloc")]
    fn split(self, byte: u8) -> Split<Self>
    where
        Self: Sized,
    {
        Split {
            buf: self,
            delim: byte,
        }
    }

    /// Returns an iterator over the lines of this reader.
    ///
    /// The iterator returned from this function will yield instances of
    /// <code>[io::Result]<[String]></code>. Each string returned will *not* have a newline
    /// byte (the `0xA` byte) or `CRLF` (`0xD`, `0xA` bytes) at the end.
    ///
    /// [io::Result]: self::Result "io::Result"
    ///
    /// # Examples
    ///
    /// [`acid_io::Cursor`][`Cursor`] is a type that implements `BufRead`. In
    /// this example, we use [`Cursor`] to iterate over all the lines in a byte
    /// slice.
    ///
    /// ```
    /// use acid_io::BufRead;
    ///
    /// let cursor = acid_io::Cursor::new(b"lorem\nipsum\r\ndolor");
    ///
    /// let mut lines_iter = cursor.lines().map(|l| l.unwrap());
    /// assert_eq!(lines_iter.next(), Some(String::from("lorem")));
    /// assert_eq!(lines_iter.next(), Some(String::from("ipsum")));
    /// assert_eq!(lines_iter.next(), Some(String::from("dolor")));
    /// assert_eq!(lines_iter.next(), None);
    /// ```
    ///
    /// # Errors
    ///
    /// Each line of the iterator has the same error semantics as [`BufRead::read_line`].
    #[cfg(feature = "alloc")]
    fn lines(self) -> Lines<Self>
    where
        Self: Sized,
    {
        Lines { buf: self }
    }
}

impl BufRead for &[u8] {
    #[inline]
    fn fill_buf(&mut self) -> Result<&[u8]> {
        Ok(*self)
    }

    #[inline]
    fn consume(&mut self, amt: usize) {
        *self = &self[amt..]
    }
}

impl<B: BufRead + ?Sized> BufRead for &mut B {
    #[inline]
    fn fill_buf(&mut self) -> Result<&[u8]> {
        (**self).fill_buf()
    }

    #[inline]
    fn consume(&mut self, amt: usize) {
        (**self).consume(amt)
    }

    #[cfg(feature = "alloc")]
    #[inline]
    fn read_until(&mut self, byte: u8, buf: &mut Vec<u8>) -> Result<usize> {
        (**self).read_until(byte, buf)
    }

    #[cfg(feature = "alloc")]
    #[inline]
    fn read_line(&mut self, buf: &mut String) -> Result<usize> {
        (**self).read_line(buf)
    }
}

impl<T> BufRead for Cursor<T>
where
    T: AsRef<[u8]>,
{
    fn fill_buf(&mut self) -> Result<&[u8]> {
        Ok(self.remaining_slice())
    }

    fn consume(&mut self, amt: usize) {
        self.pos += amt as u64;
    }
}

// Write =========================================================================================

pub(crate) fn default_write_vectored<F>(write: F, bufs: &[IoSlice<'_>]) -> Result<usize>
where
    F: FnOnce(&[u8]) -> Result<usize>,
{
    let buf = bufs
        .iter()
        .find(|b| !b.is_empty())
        .map_or(&[][..], |b| &**b);
    write(buf)
}

/// A trait for objects which are byte-oriented sinks.
///
/// Implementors of the `Write` trait are sometimes called 'writers'.
///
/// Writers are defined by two required methods, [`write`] and [`flush`]:
///
/// * The [`write`] method will attempt to write some data into the object,
///   returning how many bytes were successfully written.
///
/// * The [`flush`] method is useful for adapters and explicit buffers
///   themselves for ensuring that all buffered data has been pushed out to the
///   'true sink'.
///
/// Writers are intended to be composable with one another. Many implementors
/// throughout take and provide types which implement the `Write`
/// trait.
///
/// [`write`]: Write::write
/// [`flush`]: Write::flush
///
/// # Examples
///
/// ```
/// use acid_io::{Cursor, Write as _};
/// # fn main() -> acid_io::Result<()> {
/// let data = b"some bytes";
///
/// let mut arr = [0u8; 10];
/// let mut buffer = Cursor::new(&mut arr[..]);
/// buffer.write(data)?;
///
/// assert_eq!(buffer.get_ref(), data);
/// # Ok(())
/// # }
/// ```
///
/// The trait also provides convenience methods like [`write_all`], which calls
/// `write` in a loop until its entire input has been written.
///
/// [`write_all`]: Write::write_all
pub trait Write {
    /// Write a buffer into this writer, returning how many bytes were written.
    ///
    /// This function will attempt to write the entire contents of `buf`, but
    /// the entire write might not succeed, or the write may also generate an
    /// error. A call to `write` represents *at most one* attempt to write to
    /// any wrapped object.
    ///
    /// Calls to `write` are not guaranteed to block waiting for data to be
    /// written, and a write which would otherwise block can be indicated through
    /// an [`Err`] variant.
    ///
    /// If the return value is [`Ok(n)`] then it must be guaranteed that
    /// `n <= buf.len()`. A return value of `0` typically means that the
    /// underlying object is no longer able to accept bytes and will likely not
    /// be able to in the future as well, or that the buffer provided is empty.
    ///
    /// # Errors
    ///
    /// Each call to `write` may generate an I/O error indicating that the
    /// operation could not be completed. If an error is returned then no bytes
    /// in the buffer were written to this writer.
    ///
    /// It is **not** considered an error if the entire buffer could not be
    /// written to this writer.
    ///
    /// An error of the [`ErrorKind::Interrupted`] kind is non-fatal and the
    /// write operation should be retried if there is nothing else to do.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::prelude::*;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let mut dst = [0u8; 16];
    ///
    /// dst.as_mut_slice().write(b"some bytes")?;
    ///
    /// assert_eq!(&dst[..10], b"some bytes");
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// [`Ok(n)`]: Ok
    fn write(&mut self, src: &[u8]) -> Result<usize>;

    /// Like [`write`], except that it writes from a slice of buffers.
    ///
    /// Data is copied from each buffer in order, with the final buffer
    /// read from possibly being only partially consumed. This method must
    /// behave as a call to [`write`] with the buffers concatenated would.
    ///
    /// The default implementation calls [`write`] with either the first nonempty
    /// buffer provided, or an empty one if none exists.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::prelude::*;
    /// use acid_io::{Cursor, IoSlice};
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let mut data1 = [1; 8];
    /// let mut data2 = [15; 8];
    /// let io_slice1 = IoSlice::new(&mut data1);
    /// let io_slice2 = IoSlice::new(&mut data2);
    ///
    /// let mut dst = [0u8; 16];
    ///
    /// dst.as_mut_slice().write_vectored(&[io_slice1, io_slice2])?;
    /// assert_eq!(&dst[..8], &data1);
    /// assert_eq!(&dst[8..], &data2);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// [`write`]: Write::write
    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> Result<usize> {
        default_write_vectored(|b| self.write(b), bufs)
    }

    /// Determines if this `Write`r has an efficient [`write_vectored`]
    /// implementation.
    ///
    /// If a `Write`r does not override the default [`write_vectored`]
    /// implementation, code using it may want to avoid the method all together
    /// and coalesce writes into a single buffer for higher performance.
    ///
    /// The default implementation returns `false`.
    ///
    /// [`write_vectored`]: Write::write_vectored
    // TODO(dataphract):
    // Since this is unstable in std::io, it can't be exposed in our stable
    // public API. However, the efficiency of std::io's (and thus our) Write
    // implementations depends on it, so it's important that it be available.
    #[doc(hidden)]
    fn is_write_vectored(&self) -> bool {
        false
    }

    /// Flush this output stream, ensuring that all intermediately buffered
    /// contents reach their destination.
    ///
    /// # Errors
    ///
    /// It is considered an error if not all bytes could be written due to
    /// I/O errors or EOF being reached.
    ///
    /// # Examples
    ///
    /// ```
    /// #[cfg(not(feature = "alloc"))]
    /// # fn main() -> acid_io::Result<()> { Ok(()) }
    /// #[cfg(feature = "alloc")]
    /// # fn main() -> acid_io::Result<()> {
    /// use acid_io::prelude::*;
    /// use acid_io::BufWriter;
    ///
    /// let mut dst = [0u8; 16];
    /// let mut buffer = BufWriter::new(dst.as_mut_slice());
    ///
    /// buffer.write_all(b"some bytes")?;
    /// buffer.flush()?;
    /// drop(buffer);
    ///
    /// assert_eq!(&dst[..10], b"some bytes");
    /// # Ok(())
    /// # }
    /// ```
    fn flush(&mut self) -> Result<()>;

    /// Attempts to write an entire buffer into this writer.
    ///
    /// This method will continuously call [`write`] until there is no more data
    /// to be written or an error of non-[`ErrorKind::Interrupted`] kind is
    /// returned. This method will not return until the entire buffer has been
    /// successfully written or such an error occurs. The first error that is
    /// not of [`ErrorKind::Interrupted`] kind generated from this method will be
    /// returned.
    ///
    /// If the buffer contains no data, this will never call [`write`].
    ///
    /// # Errors
    ///
    /// This function will return the first error of
    /// non-[`ErrorKind::Interrupted`] kind that [`write`] returns.
    ///
    /// [`write`]: Write::write
    ///
    /// ```
    /// use acid_io::prelude::*;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let mut src = b"some bytes";
    ///
    /// let mut large = [0u8; 16];
    /// large.as_mut_slice().write_all(src)?;
    ///
    /// // write_all to a buffer that's not large enough
    /// let mut small = [0u8; 4];
    /// let res = small.as_mut_slice().write_all(src);
    /// assert!(res.is_err());
    ///
    /// # Ok(())
    /// # }
    /// ```
    fn write_all(&mut self, mut buf: &[u8]) -> Result<()> {
        while !buf.is_empty() {
            match self.write(buf) {
                Ok(0) => {
                    return Err(Error::new_const(
                        ErrorKind::WriteZero,
                        &"failed to write whole buffer",
                    ));
                }
                Ok(n) => buf = &buf[n..],
                Err(ref e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Attempts to write multiple buffers into this writer.
    ///
    /// This method will continuously call [`write_vectored`] until there is no
    /// more data to be written or an error of non-[`ErrorKind::Interrupted`]
    /// kind is returned. This method will not return until all buffers have
    /// been successfully written or such an error occurs. The first error that
    /// is not of [`ErrorKind::Interrupted`] kind generated from this method
    /// will be returned.
    ///
    /// If the buffer contains no data, this will never call [`write_vectored`].
    ///
    /// # Notes
    ///
    /// Unlike [`write_vectored`], this takes a *mutable* reference to
    /// a slice of [`IoSlice`]s, not an immutable one. That's because we need to
    /// modify the slice to keep track of the bytes already written.
    ///
    /// Once this function returns, the contents of `bufs` are unspecified, as
    /// this depends on how many calls to [`write_vectored`] were necessary. It is
    /// best to understand this function as taking ownership of `bufs` and to
    /// not use `bufs` afterwards. The underlying buffers, to which the
    /// [`IoSlice`]s point (but not the [`IoSlice`]s themselves), are unchanged and
    /// can be reused.
    ///
    /// [`write_vectored`]: Write::write_vectored
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(not(feature = "alloc"))]
    /// # fn main() -> acid_io::Result<()> { Ok(()) }
    /// # #[cfg(feature = "alloc")]
    /// # fn main() -> acid_io::Result<()> {
    ///
    /// use acid_io::{Write, IoSlice};
    ///
    /// let mut writer = Vec::new();
    /// let bufs = &mut [
    ///     IoSlice::new(&[1]),
    ///     IoSlice::new(&[2, 3]),
    ///     IoSlice::new(&[4, 5, 6]),
    /// ];
    ///
    /// writer.write_all_vectored(bufs)?;
    /// // Note: the contents of `bufs` is now undefined, see the Notes section.
    ///
    /// assert_eq!(writer, &[1, 2, 3, 4, 5, 6]);
    /// # Ok(()) }
    /// ```
    // TODO(dataphract):
    // LineWriter depends on this, but it's not stable, so we hide it from the
    // public API. If/when it's stabilized, remove #[doc(hidden)].
    #[doc(hidden)]
    fn write_all_vectored(&mut self, mut bufs: &mut [IoSlice<'_>]) -> Result<()> {
        // Guarantee that bufs is empty if it contains no data,
        // to avoid calling write_vectored if there is no data to be written.
        IoSlice::advance_slices(&mut bufs, 0);
        while !bufs.is_empty() {
            match self.write_vectored(bufs) {
                Ok(0) => {
                    return Err(Error::new_const(
                        ErrorKind::WriteZero,
                        &"failed to write whole buffer",
                    ));
                }
                Ok(n) => IoSlice::advance_slices(&mut bufs, n),
                Err(ref e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Writes a formatted string into this writer, returning any error
    /// encountered.
    ///
    /// This method is primarily used to interface with the
    /// [`format_args!()`] macro, and it is rare that this should
    /// explicitly be called. The [`write!()`] macro should be favored to
    /// invoke this method instead.
    ///
    /// This function internally uses the [`write_all`] method on
    /// this trait and hence will continuously write data so long as no errors
    /// are received. This also means that partial writes are not indicated in
    /// this signature.
    ///
    /// [`write_all`]: Write::write_all
    ///
    /// # Errors
    ///
    /// This function will return any I/O error reported while formatting.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::prelude::*;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let mut buffer = [0u8; 32];
    ///
    /// // this call
    /// write!(buffer.as_mut_slice(), "{:.*}", 2, 1.234567)?;
    /// // turns into this:
    /// buffer.as_mut_slice().write_fmt(format_args!("{:.*}", 2, 1.234567))?;
    /// # Ok(())
    /// # }
    /// ```
    fn write_fmt(&mut self, fmt: fmt::Arguments<'_>) -> Result<()> {
        // Create a shim which translates a Write to a fmt::Write and saves
        // off I/O errors. instead of discarding them
        struct Adapter<'a, T: ?Sized + 'a> {
            inner: &'a mut T,
            error: Result<()>,
        }

        impl<T: Write + ?Sized> fmt::Write for Adapter<'_, T> {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                match self.inner.write_all(s.as_bytes()) {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        self.error = Err(e);
                        Err(fmt::Error)
                    }
                }
            }
        }

        let mut output = Adapter {
            inner: self,
            error: Ok(()),
        };
        match fmt::write(&mut output, fmt) {
            Ok(()) => Ok(()),
            Err(..) => {
                // check if the error came from the underlying `Write` or not
                if output.error.is_err() {
                    output.error
                } else {
                    Err(Error::new_const(
                        ErrorKind::Uncategorized,
                        &"formatter error",
                    ))
                }
            }
        }
    }

    /// Creates a "by reference" adapter for this instance of `Write`.
    ///
    /// The returned adapter also implements `Write` and will simply borrow this
    /// current writer.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::Write;
    ///
    /// # fn main() -> acid_io::Result<()> {
    /// let mut buffer = [0u8; 16];
    ///
    /// let mut w = buffer.as_mut_slice();
    /// let reference = w.by_ref();
    ///
    /// // we can use reference just like our original buffer
    /// reference.write_all(b"some bytes")?;
    /// # Ok(())
    /// # }
    /// ```
    fn by_ref(&mut self) -> &mut Self
    where
        Self: Sized,
    {
        self
    }
}

impl Write for &mut [u8] {
    #[inline]
    fn write(&mut self, src: &[u8]) -> Result<usize> {
        let copy_len = cmp::min(src.len(), self.len());

        // Move slice out of self before splitting to appease borrowck.
        let (dst, rem) = mem::take(self).split_at_mut(copy_len);
        dst.copy_from_slice(&src[..copy_len]);

        *self = rem;

        Ok(copy_len)
    }

    #[inline]
    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> Result<usize> {
        let mut nwritten = 0;
        for buf in bufs {
            nwritten += self.write(buf)?;
            if self.is_empty() {
                break;
            }
        }

        Ok(nwritten)
    }

    #[inline]
    fn is_write_vectored(&self) -> bool {
        true
    }

    #[inline]
    fn write_all(&mut self, data: &[u8]) -> Result<()> {
        if self.write(data)? == data.len() {
            Ok(())
        } else {
            Err(Error::new_const(
                ErrorKind::WriteZero,
                &"failed to write whole buffer",
            ))
        }
    }

    #[inline]
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

// Seek ==========================================================================================

/// Enumeration of possible methods to seek within an I/O object.
///
/// It is used by the [`Seek`] trait.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SeekFrom {
    /// Sets the offset to the provided number of bytes.
    Start(u64),
    /// Sets the offset to the size of this object plus the specified number of
    /// bytes.
    ///
    /// It is possible to seek beyond the end of an object, but it's an error to
    /// seek before byte 0.
    End(i64),
    /// Sets the offset to the current position plus the specified number of
    /// bytes.
    ///
    /// It is possible to seek beyond the end of an object, but it's an error to
    /// seek before byte 0.
    Current(i64),
}

/// The `Seek` trait provides a cursor which can be moved within a stream of
/// bytes.
///
/// The stream typically has a fixed size, allowing seeking relative to either
/// end or the current offset.
pub trait Seek {
    /// Seek to an offset, in bytes, in a stream.
    ///
    /// A seek beyond the end of a stream is allowed, but behavior is defined
    /// by the implementation.
    ///
    /// If the seek operation completed successfully,
    /// this method returns the new position from the start of the stream.
    /// That position can be used later with [`SeekFrom::Start`].
    ///
    /// # Errors
    ///
    /// Seeking can fail, for example because it might involve flushing a buffer.
    ///
    /// Seeking to a negative offset is considered an error.
    fn seek(&mut self, pos: SeekFrom) -> Result<u64>;

    /// Rewind to the beginning of a stream.
    ///
    /// This is a convenience method, equivalent to `seek(SeekFrom::Start(0))`.
    ///
    /// # Errors
    ///
    /// Rewinding can fail, for example because it might involve flushing a buffer.
    fn rewind(&mut self) -> Result<()> {
        self.seek(SeekFrom::Start(0))?;
        Ok(())
    }

    /// Returns the length of this stream (in bytes).
    ///
    /// This method is implemented using up to three seek operations. If this
    /// method returns successfully, the seek position is unchanged (i.e. the
    /// position before calling this method is the same as afterwards).
    /// However, if this method returns an error, the seek position is
    /// unspecified.
    ///
    /// If you need to obtain the length of *many* streams and you don't care
    /// about the seek position afterwards, you can reduce the number of seek
    /// operations by simply calling `seek(SeekFrom::End(0))` and using its
    /// return value (it is also the stream length).
    ///
    /// Note that length of a stream can change over time (for example, when
    /// data is appended to a file). So calling this method multiple times does
    /// not necessarily return the same length each time.
    fn stream_len(&mut self) -> Result<u64> {
        let old_pos = self.stream_position()?;
        let len = self.seek(SeekFrom::End(0))?;

        // Avoid seeking a third time when we were already at the end of the
        // stream. The branch is usually way cheaper than a seek operation.
        if old_pos != len {
            self.seek(SeekFrom::Start(old_pos))?;
        }

        Ok(len)
    }

    /// Returns the current seek position from the start of the stream.
    ///
    /// This is equivalent to `self.seek(SeekFrom::Current(0))`.
    fn stream_position(&mut self) -> Result<u64> {
        self.seek(SeekFrom::Current(0))
    }
}

// Cursor ========================================================================================

/// A `Cursor` wraps an in-memory buffer and provides it with a
/// [`Seek`] implementation.
///
/// `Cursor`s are used with in-memory buffers, anything implementing
/// `AsRef<u8>`, to allow them to implement [`Read`] and/or [`Write`], allowing
/// these buffers to be used anywhere you might use a reader or writer that does
/// actual I/O.
///
/// `acid_io` implements some I/O traits on various types which are commonly
/// used as a buffer, like `Cursor<Vec<u8>>` and `Cursor<&[u8]>`.
///
/// # Examples
///
/// [bytes]: crate::slice "slice"
///
/// ```
/// use acid_io::{self, Read, Seek, SeekFrom, Write};
///
/// // a library function we've written
/// fn write_ten_bytes_at_end<W: Write + Seek>(writer: &mut W) -> acid_io::Result<()> {
///     writer.seek(SeekFrom::End(-10))?;
///
///     for i in 0..10 {
///         writer.write(&[i])?;
///     }
///
///     // all went well
///     Ok(())
/// }
///
/// // now let's write a test
/// #[test]
/// fn test_writes_bytes() {
///     use acid_io::Cursor;
///     let mut buf = Cursor::new(vec![0; 15]);
///
///     write_ten_bytes_at_end(&mut buf).unwrap();
///
///     assert_eq!(&buf.get_ref()[5..15], &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
/// }
/// ```
#[derive(Debug, Default, Eq, PartialEq)]
pub struct Cursor<T> {
    pub(crate) inner: T,
    pub(crate) pos: u64,
}

impl<T> Cursor<T> {
    /// Creates a new cursor wrapping the provided underlying in-memory buffer.
    ///
    /// The initial position of the cursor is `0` even if the underlying buffer
    /// is not empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::Cursor;
    ///
    /// let buf = Cursor::new(Vec::new());
    /// # fn force_inference(_: &Cursor<Vec<u8>>) {}
    /// # force_inference(&buf);
    /// ```
    pub const fn new(inner: T) -> Cursor<T> {
        Cursor { inner, pos: 0 }
    }

    /// Consumes this cursor, returning the underlying value.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::Cursor;
    ///
    /// let buf = Cursor::new(Vec::new());
    /// # fn force_inference(_: &Cursor<Vec<u8>>) {}
    /// # force_inference(&buf);
    ///
    /// let vec = buf.into_inner();
    /// ```
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// Gets a reference to the underlying value in this cursor.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::Cursor;
    ///
    /// let buf = Cursor::new(Vec::new());
    /// # fn force_inference(_: &Cursor<Vec<u8>>) {}
    /// # force_inference(&buf);
    ///
    /// let reference = buf.get_ref();
    /// ```
    pub const fn get_ref(&self) -> &T {
        &self.inner
    }

    /// Gets a mutable reference to the underlying value in this cursor.
    ///
    /// Care should be taken to avoid modifying the internal I/O state of the
    /// underlying value as it may corrupt this cursor's position.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::Cursor;
    ///
    /// let mut buf = Cursor::new(Vec::new());
    /// # fn force_inference(_: &Cursor<Vec<u8>>) {}
    /// # force_inference(&buf);
    ///
    /// let reference = buf.get_mut();
    /// ```
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// Returns the current position of this cursor.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::prelude::*;
    /// use acid_io::{Cursor, SeekFrom};
    ///
    /// let mut buf = Cursor::new(vec![1, 2, 3, 4, 5]);
    ///
    /// assert_eq!(buf.position(), 0);
    ///
    /// buf.seek(SeekFrom::Current(2)).unwrap();
    /// assert_eq!(buf.position(), 2);
    ///
    /// buf.seek(SeekFrom::Current(-1)).unwrap();
    /// assert_eq!(buf.position(), 1);
    /// ```
    pub const fn position(&self) -> u64 {
        self.pos
    }

    /// Sets the position of this cursor.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::Cursor;
    ///
    /// let mut buf = Cursor::new(vec![1, 2, 3, 4, 5]);
    ///
    /// assert_eq!(buf.position(), 0);
    ///
    /// buf.set_position(2);
    /// assert_eq!(buf.position(), 2);
    ///
    /// buf.set_position(4);
    /// assert_eq!(buf.position(), 4);
    /// ```
    pub fn set_position(&mut self, pos: u64) {
        self.pos = pos;
    }
}

impl<T> Cursor<T>
where
    T: AsRef<[u8]>,
{
    /// Returns the remaining slice.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::Cursor;
    ///
    /// let mut buf = Cursor::new(vec![1, 2, 3, 4, 5]);
    ///
    /// assert_eq!(buf.remaining_slice(), &[1, 2, 3, 4, 5]);
    ///
    /// buf.set_position(2);
    /// assert_eq!(buf.remaining_slice(), &[3, 4, 5]);
    ///
    /// buf.set_position(4);
    /// assert_eq!(buf.remaining_slice(), &[5]);
    ///
    /// buf.set_position(6);
    /// assert_eq!(buf.remaining_slice(), &[]);
    /// ```
    pub fn remaining_slice(&self) -> &[u8] {
        let len = self.pos.min(self.inner.as_ref().len() as u64);
        &self.inner.as_ref()[(len as usize)..]
    }

    /// Returns `true` if the remaining slice is empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use acid_io::Cursor;
    ///
    /// let mut buf = Cursor::new(vec![1, 2, 3, 4, 5]);
    ///
    /// buf.set_position(2);
    /// assert!(!buf.is_empty());
    ///
    /// buf.set_position(5);
    /// assert!(buf.is_empty());
    ///
    /// buf.set_position(10);
    /// assert!(buf.is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.pos >= self.inner.as_ref().len() as u64
    }
}

impl<T> Read for Cursor<T>
where
    T: AsRef<[u8]>,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let n = Read::read(&mut self.remaining_slice(), buf)?;
        self.pos += n as u64;
        Ok(n)
    }

    fn read_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> Result<usize> {
        let mut nread = 0;
        for buf in bufs {
            let n = self.read(buf)?;
            nread += n;
            if n < buf.len() {
                break;
            }
        }
        Ok(nread)
    }

    fn is_read_vectored(&self) -> bool {
        true
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        let n = buf.len();
        Read::read_exact(&mut self.remaining_slice(), buf)?;
        self.pos += n as u64;
        Ok(())
    }
}

// Non-resizing write implementation
#[inline]
pub(crate) fn slice_write(pos_mut: &mut u64, slice: &mut [u8], buf: &[u8]) -> Result<usize> {
    let pos = cmp::min(*pos_mut, slice.len() as u64);
    let amt = (&mut slice[(pos as usize)..]).write(buf)?;
    *pos_mut += amt as u64;
    Ok(amt)
}

#[inline]
pub(crate) fn slice_write_vectored(
    pos_mut: &mut u64,
    slice: &mut [u8],
    bufs: &[IoSlice<'_>],
) -> Result<usize> {
    let mut nwritten = 0;
    for buf in bufs {
        let n = slice_write(pos_mut, slice, buf)?;
        nwritten += n;
        if n < buf.len() {
            break;
        }
    }
    Ok(nwritten)
}

impl Write for Cursor<&mut [u8]> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        slice_write(&mut self.pos, self.inner, buf)
    }

    #[inline]
    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> Result<usize> {
        slice_write_vectored(&mut self.pos, self.inner, bufs)
    }

    #[inline]
    fn is_write_vectored(&self) -> bool {
        true
    }

    #[inline]
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

impl<A> Seek for Cursor<A>
where
    A: AsRef<[u8]>,
{
    fn seek(&mut self, style: SeekFrom) -> Result<u64> {
        let (base_pos, offset) = match style {
            SeekFrom::Start(n) => {
                self.pos = n;
                return Ok(n);
            }
            SeekFrom::End(n) => (self.inner.as_ref().len() as u64, n),
            SeekFrom::Current(n) => (self.pos, n),
        };

        // TODO: The standard library does this whole op with a call to
        // `checked_add_signed`, which is unstable behind `mixed_integer_ops`
        // (https://github.com/rust-lang/rust/issues/87840). Once (if) that
        // stabilizes, follow suit here.
        let end_pos = {
            let (p, overflowed) = base_pos.overflowing_add(offset as u64);

            // If offset < 0 and the unsigned addition didn't overflow, then
            // the signed addition would have a negative sum.
            if overflowed ^ (offset < 0) {
                None
            } else {
                Some(p)
            }
        };

        match end_pos {
            Some(n) => {
                self.pos = n;
                Ok(self.pos)
            }
            None => Err(Error::new_const(
                ErrorKind::InvalidInput,
                &"invalid seek to a negative or overflowing position",
            )),
        }
    }

    fn stream_len(&mut self) -> Result<u64> {
        Ok(self.inner.as_ref().len() as u64)
    }

    fn stream_position(&mut self) -> Result<u64> {
        Ok(self.pos)
    }
}
