//! Reading files from the EFI System Partition we were loaded from.

use alloc::vec;
use alloc::vec::Vec;

use uefi::boot;
use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode, RegularFile};
use uefi::{CStr16, Status};

/// Read an entire file from the root of the volume this image was loaded
/// from.
///
/// Returns [`Status::NOT_FOUND`] if the file does not exist,
/// [`Status::INVALID_PARAMETER`] if the name refers to a directory, and
/// [`Status::END_OF_FILE`] if the file is shorter than its own reported
/// size.
pub fn read_file(name: &CStr16) -> Result<Vec<u8>, Status> {
    // The image handle identifies the device we were booted from, so
    // this lands on the same ESP that holds BOOTX64.EFI.
    let mut fs = boot::get_image_file_system(boot::image_handle())
        .map_err(|e| e.status())?;
    let mut root = fs.open_volume().map_err(|e| e.status())?;

    let handle = root
        .open(name, FileMode::Read, FileAttribute::empty())
        .map_err(|e| e.status())?;

    // `open` succeeds for directories too; reading one as a regular file
    // is undefined, so reject it explicitly rather than trusting the name.
    let mut file: RegularFile = handle
        .into_regular_file()
        .ok_or(Status::INVALID_PARAMETER)?;

    // Ask the file how big it is rather than guessing a buffer size.
    let info = file.get_boxed_info::<FileInfo>().map_err(|e| e.status())?;
    let size = info.file_size() as usize;

    let mut buffer = vec![0u8; size];
    let mut filled = 0usize;
    while filled < size {
        let read = file.read(&mut buffer[filled..]).map_err(|e| e.status())?;
        if read == 0 {
            // EOF before we got the bytes the file's own metadata promised.
            // Returning a short buffer here would surface later as a
            // baffling ELF parse error, so fail where the cause is obvious.
            return Err(Status::END_OF_FILE);
        }
        filled += read;
    }

    Ok(buffer)
}
