//! UI-independent core of the file explorer.
//!
//! Architecture rule (plan §4): the UI never touches the disk. Everything in
//! `core` runs either on background threads or is pure data manipulation, and
//! results are pushed back to the UI thread by the caller.

pub mod core;
