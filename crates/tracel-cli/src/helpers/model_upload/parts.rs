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
                "Upload response for '{rel_path}' has non-contiguous or duplicate part numbers (expected part {expected}, got {}).",
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
            "Upload response for '{rel_path}' parts sum to {total} bytes but local file is {expected_size} bytes."
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
                "Upload response referenced unknown file path '{}'.",
                upload_file.rel_path
            )
        })?;
        let expected_size = *file_sizes.get(&upload_file.rel_path).ok_or_else(|| {
            anyhow::anyhow!(
                "Upload response referenced file '{}' with no known local size.",
                upload_file.rel_path
            )
        })?;

        if upload_file.urls.parts.is_empty() {
            anyhow::bail!(
                "Upload response for '{}' does not contain any part.",
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
    use super::*;
    use tracel_client::console::artifact::response::MultipartUploadResponse;

    fn local_file(size: u64) -> (BTreeMap<String, PathBuf>, BTreeMap<String, u64>) {
        let files = BTreeMap::from([("weights.bin".to_string(), PathBuf::from("weights.bin"))]);
        let sizes = BTreeMap::from([("weights.bin".to_string(), size)]);
        (files, sizes)
    }

    fn response(rel_path: &str, parts: &[(u32, u64)]) -> Vec<PresignedModelFileUploadUrlsResponse> {
        vec![PresignedModelFileUploadUrlsResponse {
            rel_path: rel_path.to_string(),
            urls: MultipartUploadResponse {
                id: "upload-1".to_string(),
                parts: parts
                    .iter()
                    .map(|&(part, size_bytes)| PresignedUploadUrlResponse {
                        part,
                        url: format!("https://example.com/part{part}"),
                        size_bytes,
                    })
                    .collect(),
            },
        }]
    }

    #[test]
    fn parts_get_sequential_offsets_in_part_order() {
        let (files, sizes) = local_file(300);

        let tasks = build_part_tasks(
            &files,
            &sizes,
            &response("weights.bin", &[(2, 100), (1, 200)]),
        )
        .unwrap();

        let layout: Vec<(u32, u64, u64)> = tasks
            .iter()
            .map(|task| (task.part, task.offset, task.size_bytes))
            .collect();
        assert_eq!(layout, [(1, 0, 200), (2, 200, 100)]);
    }

    #[test]
    fn an_upload_response_that_does_not_cover_the_file_is_rejected() {
        let (files, sizes) = local_file(300);
        for (rel_path, parts, named) in [
            ("missing.bin", &[(1, 300)][..], "'missing.bin'"),
            ("weights.bin", &[], "'weights.bin'"),
            ("weights.bin", &[(1, 200)], "200 bytes"),
            ("weights.bin", &[(1, 150), (1, 150)], "got 1"),
            ("weights.bin", &[(1, 200), (3, 100)], "got 3"),
        ] {
            let error = build_part_tasks(&files, &sizes, &response(rel_path, parts)).unwrap_err();
            assert!(error.to_string().contains(named), "{parts:?}: {error}");
        }
    }
}
