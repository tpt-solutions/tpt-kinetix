$c = Get-Content tpt-kinetix-aac/src/syntax.rs -Raw
$old = "7 => {
                    elements.push(Element::End);
                    break;
                }
                // CCE (2), DSE (4), PCE (5): out of Phase 1 scope.
                2 | 4 | 5 => {
                    // CCE (2) / DSE (4) / PCE (5) are out of the AAC-LC
                    // reconstruction scope, but real streams legitimately carry
                    // them (e.g. ffmpeg emits a PCE in the first ADTS frame for
                    // many channel configs). Skip them rather than failing the
                    // whole raw-data-block parse, so the decoder can still
                    // reconstruct the channels it does understand.
                    let _tag = reader.read_bits(4).ok.or(AacParseError::UnexpectedEof)? as u8;
                    if id == 4 {
                        skip_data_stream_element(&mut reader)?;
                    } else if id == 5 {
                        skip_program_config_element(&mut reader)?;
                    } else {
                        return Err(AacParseError::Unsupported(
                            \"CCE element parsing is not in Phase 1 scope\",
                        ));
                    }
                }
                _ => return Err(AacParseError::BadElementId),"
$new = "7 => {
                    elements.push(Element::End);
                    break;
                }
                2 => {
                    let tag = reader.read_bits(4).ok.or(AacParseError::UnexpectedEof)? as u8;
                    let common_window = reader.read_bit().ok.or(AacParseError::UnexpectedEof)? != 0;
                    let shared = if common_window {
                        Some(IcsInfo::parse(&mut reader)?)
                    } else {
                        None
                    };
                    // num_gain_element_lists (4 bits)
                    let num_gain_element_lists = reader.read_bits(4).ok.or(AacParseError::UnexpectedEof)? as u8;
                    let mut gain_element_lists = Vec::with_capacity(num_gain_element_lists as usize);
                    for _ in 0..num_gain_element_lists {
                        // gain_element_scale (1 bit)
                        let gain_element_scale = reader.read_bit().ok.or(AacParseError::UnexpectedEof)? != 0;
                        // num_gain_elements (4 bits)
                        let num_gain_elements = reader.read_bits(4).ok.or(AacParseError::UnexpectedEof)? as u8;
                        let mut gain_elements = Vec::with_capacity(num_gain_elements as usize);
                        for _ in 0..num_gain_elements {
                            // cce_gain (3 bits)
                            let cce_gain = reader.read_bits(3).ok.or(AacParseError::UnexpectedEof)? as u8;
                            // cce_scale (4 bits)
                            let cce_scale = reader.read_bits(4).ok.or(AacParseError::UnexpectedEof)? as u8;
                            // target_tag (4 bits)
                            let target_tag = reader.read_bits(4).ok.or(AacParseError::UnexpectedEof)? as u8;
                            // gain_element_index (4 bits)
                            let gain_element_index = reader.read_bits(4).ok.or(AacParseError::UnexpectedEof)? as u8;
                            gain_elements.push(GainElement {
                                cce_gain,
                                cce_scale,
                                target_tag,
                                gain_element_index,
                            });
                        }
                        gain_element_lists.push(GainElementList {
                            gain_element_scale,
                            num_gain_elements,
                            gain_elements,
                        });
                    }
                    elements.push(Element::Cce(CouplingChannelElement {
                        instance_tag: tag,
                        common_window,
                        ics: shared,
                        num_gain_element_lists,
                        gain_element_lists,
                    }));
                }
                4 | 5 => {
                    // DSE (4) / PCE (5): skip.
                    let _tag = reader.read_bits(4).ok.or(AacParseError::UnexpectedEof)? as u8;
                    if id == 4 {
                        skip_data_stream_element(&mut reader)?;
                    } else if id == 5 {
                        skip_program_config_element(&mut reader)?;
                    }
                }
                _ => return Err(AacParseError::BadElementId),"
$c = $c -replace [regex]::Escape($old), $new
Set-Content tpt-kinetix-aac/src/syntax.rs -Value $c