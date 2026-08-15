$c = Get-Content tpt-kinetix-aac/src/syntax.rs -Raw
$old = "2 => {
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
                }"
$new = "2 => {
                    let tag = reader.read_bits(4).ok.or(AacParseError::UnexpectedEof)? as u8;
                    let common_window = reader.read_bit().ok.or(AacParseError::UnexpectedEof)? != 0;
                    let shared = if common_window {
                        Some(IcsInfo::parse(&mut reader)?)
                    } else {
                        None
                    };
                    // num_gain_element_lists (4 bits) - transmitted as N-1, so actual count is +1
                    let num_gain_element_lists = reader.read_bits(4).ok.or(AacParseError::UnexpectedEof)? as u8;
                    let mut gain_element_lists = Vec::with_capacity((num_gain_element_lists + 1) as usize);
                    for _ in 0..=num_gain_element_lists {
                        // gain_element_scale (1 bit)
                        let gain_element_scale = reader.read_bit().ok.or(AacParseError::UnexpectedEof)? != 0;
                        // num_gain_elements (4 bits) - transmitted as N-1
                        let num_gain_elements = reader.read_bits(4).ok.or(AacParseError::UnexpectedEof)? as u8;
                        let mut gain_elements = Vec::with_capacity((num_gain_elements + 1) as usize);
                        for _ in 0..=num_gain_elements {
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
                }"
$c = $c -replace [regex]::Escape($old), $new
Set-Content tpt-kinetix-aac/src/syntax.rs -Value $c