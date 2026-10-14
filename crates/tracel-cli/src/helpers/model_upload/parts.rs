use std::collections::BTreeMap;
use std::path::PathBuf;
use tracel_client::console::artifact::response::PresignedUploadUrlResponse;
use tracel_client::console::model::response::PresignedModelFileUploadUrlsResponse;

/// One part of a file to upload: where its bytes are in the file, and the URL to upload
/// them to.
#[derive(Clone, Debug)]
pub struct PartUploadTask {
    pub rel_path: String,
    pub absolute_path: PathBuf,
    pub part: u32,
    pub url: String,
    pub offset: u64,
    pub size_bytes: u64,
}

fn validate_part_sequence(
    rel_path: &str,
    parts: &[PresignedUploadUrlResponse],
) -> anyhow::Result<()> {
    for (index, part) in parts.iter().enumerate() {
        let expected = index as u32 + 1;
        if part.part != expected {
            anyhow::bail!(
                "The Tracel Console numbered the parts of '{rel_path}' wrongly: expected part {expected}, got {}.",
                part.part
            );
        }
    }
    Ok(())
}

fn validate_part_total_size(
    rel_path: &str,
    parts: &[PresignedUploadUrlResponse],
    expected_size: u64,
) -> anyhow::Result<()> {
    let total: u64 = parts.iter().map(|part| part.size_bytes).sum();
    if total != expected_size {
        anyhow::bail!(
            "The Tracel Console's parts of '{rel_path}' sum to {total} bytes, not {expected_size}."
        );
    }
    Ok(())
}

/// Split each file of an upload response into its parts, checking that they number from
/// 1 and cover the file.
pub fn build_part_tasks(
    files: &BTreeMap<String, PathBuf>,
    file_sizes: &BTreeMap<String, u64>,
    upload_files: &[PresignedModelFileUploadUrlsResponse],
) -> anyhow::Result<Vec<PartUploadTask>> {
    let mut tasks = Vec::new();

    for upload_file in upload_files {
        let absolute_path = files.get(&upload_file.rel_path).ok_or_else(|| {
            anyhow::anyhow!(
                "The Tracel Console sent parts of an unknown file '{}'.",
                upload_file.rel_path
            )
        })?;
        let expected_size = *file_sizes.get(&upload_file.rel_path).ok_or_else(|| {
            anyhow::anyhow!(
                "The Tracel Console sent parts of '{}', whose size is unknown.",
                upload_file.rel_path
            )
        })?;

        if upload_file.urls.parts.is_empty() {
            anyhow::bail!(
                "The Tracel Console sent no parts of '{}'.",
                upload_file.rel_path
            );
        }
        let mut parts = upload_file.urls.parts.clone();
        parts.sort_by_key(|part| part.part);
        validate_part_sequence(&upload_file.rel_path, &parts)?;
        validate_part_total_size(&upload_file.rel_path, &parts, expected_size)?;

        let mut offset = 0u64;
        for part in parts {
            tasks.push(PartUploadTask {
                rel_path: upload_file.rel_path.clone(),
                absolute_path: absolute_path.clone(),
                part: part.part,
                url: part.url,
                offset,
                size_bytes: part.size_bytes,
            });
            offset += part.size_bytes;
        }
    }

    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use tracel_client::console::artifact::response::MultipartUploadResponse;

    use super::*;

    fn weights(size: u64) -> (BTreeMap<String, PathBuf>, BTreeMap<String, u64>) {
        let files = BTreeMap::from([("weights.bin".to_string(), PathBuf::from("weights.bin"))]);
        let sizes = BTreeMap::from([("weights.bin".to_string(), size)]);
        (files, sizes)
    }

    fn parts_of(rel_path: &str, parts: &[(u32, u64)]) -> Vec<PresignedModelFileUploadUrlsResponse> {
        vec![PresignedModelFileUploadUrlsResponse {
            rel_path: rel_path.to_string(),
            urls: MultipartUploadResponse {
                id: "upload-1".to_string(),
                parts: parts
                    .iter()
                    .map(|&(part, size_bytes)| PresignedUploadUrlResponse {
                        part,
                        url: format!("https://storage/part{part}"),
                        size_bytes,
                    })
                    .collect(),
            },
        }]
    }

    #[test]
    fn given_parts_out_of_order_when_splitting_a_file_then_each_part_starts_where_the_previous_ends()
     {
        let (files, sizes) = weights(300);

        let tasks = build_part_tasks(
            &files,
            &sizes,
            &parts_of("weights.bin", &[(2, 100), (1, 200)]),
        )
        .unwrap();

        assert_eq!(
            (tasks[0].part, tasks[0].offset, tasks[0].size_bytes),
            (1, 0, 200)
        );
        assert_eq!(
            (tasks[1].part, tasks[1].offset, tasks[1].size_bytes),
            (2, 200, 100)
        );
    }

    #[rstest]
    #[case::unknown_file("other.bin", &[(1, 300)], "'other.bin'")]
    #[case::no_parts("weights.bin", &[], "no parts")]
    #[case::too_small("weights.bin", &[(1, 200)], "200 bytes")]
    #[case::repeated_part("weights.bin", &[(1, 150), (1, 150)], "got 1")]
    #[case::missing_part("weights.bin", &[(1, 200), (3, 100)], "got 3")]
    fn given_parts_that_do_not_cover_the_file_when_splitting_it_then_the_upload_is_refused_for_that_reason(
        #[case] rel_path: &str,
        #[case] parts: &[(u32, u64)],
        #[case] reason: &str,
    ) {
        let (files, sizes) = weights(300);

        let error = build_part_tasks(&files, &sizes, &parts_of(rel_path, parts)).unwrap_err();

        assert!(error.to_string().contains(reason), "{error}");
    }
}
