$content = Get-Content tpt-kinetix-aac/src/syntax.rs -Raw
# Fix 1: Add byte_align after FIL payload
$content = $content -replace '(let mut payload = Vec::with_capacity\(count\);\\s*for _ in 0\.\.count \{\\s*payload\.push\(reader\.read_u8\(\)\.ok_or\(AacParseError::UnexpectedEof\)\?\);\s*\})', '$1
                    // fill_element() ends with byte_alignment() per ISO 14496-3.
                    reader.byte_align();')
# Fix 2: Tolerate EOF at element boundary
$content = $content -replace '(let id = reader\.read_bits\(3\)\.ok_or\(AacParseError::UnexpectedEof\)\? as u8;)', 'let id = match reader.read_bits(3) {
                Some(id) => id as u8,
                None => {
                    // Tolerate missing END element (id=7) at frame boundary.
                    // Some encoders (ffmpeg) omit the explicit END and rely on
                    // the ADTS frame_length to delimit the raw_data_block.
                    break;
                }
            };')
Set-Content tpt-kinetix-aac/src/syntax.rs -Value $content