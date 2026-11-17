//! Core of a TI Connect CE work-alike.
//!
//! * [`tokens`]  – TI-BASIC token table (from TI Connect CE's own dictionary) plus
//!                 tokenizer / detokenizer.
//! * [`varfile`] – reading and writing `.8xp`, `.8xv`, `.8xl`, … (`**TI83F*`) files and
//!                 decoding variable contents (reals, lists, matrices, strings, programs).
//! * [`dusb`]    – the DUSB protocol ("CARS" inside TI's NativeConnectServer.dll) used by the
//!                 TI-84 Plus / CE family over USB: raw packets, virtual packets, commands.
//! * [`usb`]     – a libusb transport for [`dusb`].
//! * [`screen`]  – screenshot decoding and PNG output.
//! * [`backup`]  – whole-calculator backup/restore and clock sync.

pub mod backup;
pub mod basic;
pub mod dusb;
pub mod flash;
pub mod info;
pub mod model;
pub mod osupdate;
pub mod screen;
pub mod tokens;
pub mod usb;
pub mod varfile;
