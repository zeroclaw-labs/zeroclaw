#[cfg(test)]
mod tests {
    use crate::skills::skills_dir;
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn test_skills_symlink_unix_edge_cases() {
        let tmp = TempDir::new().unwrap();
        let workspace_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();

        let skills_path = skills_dir(&workspace_dir);
        std::fs::create_dir_all(&skills_path).unwrap();

        // Test case 1: Valid symlink creation on Unix
        #[cfg(unix)]
        {
            let source_dir = tmp.path().join("source_skill");
            std::fs::create_dir_all(&source_dir).unwrap();
            std::fs::write(source_dir.join("SKILL.md"), "# Test Skill\nContent").unwrap();

            let dest_link = skills_path.join("linked_skill");

            // Create symlink
            let result = std::os::unix::fs::symlink(&source_dir, &dest_link);
            assert!(result.is_ok(), "Symlink creation should succeed");

            // Verify symlink works
            assert!(dest_link.exists());
            assert!(dest_link.is_symlink());

            // Verify we can read through symlink
            let content = std::fs::read_to_string(dest_link.join("SKILL.md"));
            assert!(content.is_ok());
            assert!(content.unwrap().contains("Test Skill"));

            // Test case 2: Symlink to non-existent target should fail gracefully
            let broken_link = skills_path.join("broken_skill");
            let non_existent = tmp.path().join("non_existent");
            let result = std::os::unix::fs::symlink(&non_existent, &broken_link);
            assert!(
                result.is_ok(),
                "Symlink creation should succeed even if target doesn't exist"
            );

            // But reading through it should fail
            let content = std::fs::read_to_string(broken_link.join("SKILL.md"));
            assert!(content.is_err());
        }

        // Test case 3: Non-Unix platforms should handle symlink errors gracefully
        #[cfg(not(unix))]
        {
            let source_dir = tmp.path().join("source_skill");
            std::fs::create_dir_all(&source_dir).unwrap();

            let dest_link = skills_path.join("linked_skill");

            // Symlink should fail on non-Unix
            let result = std::os::unix::fs::symlink(&source_dir, &dest_link);
            assert!(result.is_err());

            // Directory should not exist
            assert!(!dest_link.exists());
        }

        // Test case 4: skills_dir function edge cases
        let workspace_with_trailing_slash = format!("{}/", workspace_dir.display());
        let path_from_str = skills_dir(Path::new(&workspace_with_trailing_slash));
        assert_eq!(path_from_str, skills_path);

        // Test case 5: Empty workspace directory
        let empty_workspace = tmp.path().join("empty");
        let empty_skills_path = skills_dir(&empty_workspace);
        assert_eq!(empty_skills_path, empty_workspace.join("skills"));
        assert!(!empty_skills_path.exists());
    }

    #[test]
    fn test_skills_symlink_permissions_and_safety() {
        let tmp = TempDir::new().unwrap();
        let workspace_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();

        let skills_path = skills_dir(&workspace_dir);
        std::fs::create_dir_all(&skills_path).unwrap();

        #[cfg(unix)]
        {
            // Test case: Symlink outside workspace should be allowed (user responsibility)
            let outside_dir = tmp.path().join("outside_skill");
            std::fs::create_dir_all(&outside_dir).unwrap();
            std::fs::write(outside_dir.join("SKILL.md"), "# Outside Skill\nContent").unwrap();

            let dest_link = skills_path.join("outside_skill");
            let result = std::os::unix::fs::symlink(&outside_dir, &dest_link);
            assert!(
                result.is_ok(),
                "Should allow symlinking to directories outside workspace"
            );

            // Should still be readable
            let content = std::fs::read_to_string(dest_link.join("SKILL.md"));
            assert!(content.is_ok());
            assert!(content.unwrap().contains("Outside Skill"));
        }
    }
}
