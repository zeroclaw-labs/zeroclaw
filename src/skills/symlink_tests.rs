#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::skills::{handle_command, load_skills_with_config, skills_dir};
    use crate::SkillCommands;
    use std::path::Path;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_skills_symlink_unix_edge_cases() {
        let tmp = TempDir::new().unwrap();
        let workspace_dir = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&workspace_dir).await.unwrap();

        let skills_path = skills_dir(&workspace_dir);
        tokio::fs::create_dir_all(&skills_path).await.unwrap();

        // Test case 1: Valid symlink creation on Unix
        #[cfg(unix)]
        {
            let source_dir = tmp.path().join("source_skill");
            tokio::fs::create_dir_all(&source_dir).await.unwrap();
            tokio::fs::write(source_dir.join("SKILL.md"), "# Test Skill\nContent")
                .await
                .unwrap();

            let dest_link = skills_path.join("linked_skill");

            // Create symlink
            let result = std::os::unix::fs::symlink(&source_dir, &dest_link);
            assert!(result.is_ok(), "Symlink creation should succeed");

            // Verify symlink works
            assert!(dest_link.exists());
            assert!(dest_link.is_symlink());

            // Verify we can read through symlink
            let content = tokio::fs::read_to_string(dest_link.join("SKILL.md")).await;
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
            let content = tokio::fs::read_to_string(broken_link.join("SKILL.md")).await;
            assert!(content.is_err());
        }

        // Test case 3: Non-Unix platforms should handle symlink errors gracefully
        #[cfg(windows)]
        {
            let source_dir = tmp.path().join("source_skill");
            tokio::fs::create_dir_all(&source_dir).await.unwrap();

            let dest_link = skills_path.join("linked_skill");

            // On Windows, creating directory symlinks may require elevated privileges
            let result = std::os::windows::fs::symlink_dir(&source_dir, &dest_link);
            // If symlink creation fails (no privileges), the directory should not exist
            if result.is_err() {
                assert!(!dest_link.exists());
            } else {
                // Clean up if it succeeded
                let _ = tokio::fs::remove_dir(&dest_link).await;
            }
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

    #[tokio::test]
    async fn test_workspace_symlink_loading_requires_trusted_roots() {
        let tmp = TempDir::new().unwrap();
        let workspace_dir = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&workspace_dir).await.unwrap();

        let skills_path = skills_dir(&workspace_dir);
        tokio::fs::create_dir_all(&skills_path).await.unwrap();

        #[cfg(unix)]
        {
            let outside_dir = tmp.path().join("outside_skill");
            tokio::fs::create_dir_all(&outside_dir).await.unwrap();
            tokio::fs::write(outside_dir.join("SKILL.md"), "# Outside Skill\nContent")
                .await
                .unwrap();

            let dest_link = skills_path.join("outside_skill");
            let result = std::os::unix::fs::symlink(&outside_dir, &dest_link);
            assert!(result.is_ok(), "symlink creation should succeed on unix");

            let mut config = Config::default();
            config.workspace_dir = workspace_dir.clone();
            config.config_path = workspace_dir.join("config.toml");

            let blocked = load_skills_with_config(&workspace_dir, &config);
            assert!(
                blocked.is_empty(),
                "symlinked skill should be rejected when trusted_skill_roots is empty"
            );

            config.skills.trusted_skill_roots = vec![tmp.path().display().to_string()];
            let allowed = load_skills_with_config(&workspace_dir, &config);
            assert_eq!(
                allowed.len(),
                1,
                "symlinked skill should load when target is inside trusted roots"
            );
            assert_eq!(allowed[0].name, "outside_skill");
        }
    }

    #[tokio::test]
    async fn test_skills_audit_respects_trusted_symlink_roots() {
        let tmp = TempDir::new().unwrap();
        let workspace_dir = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&workspace_dir).await.unwrap();

        let skills_path = skills_dir(&workspace_dir);
        tokio::fs::create_dir_all(&skills_path).await.unwrap();

        #[cfg(unix)]
        {
            let outside_dir = tmp.path().join("outside_skill");
            tokio::fs::create_dir_all(&outside_dir).await.unwrap();
            tokio::fs::write(outside_dir.join("SKILL.md"), "# Outside Skill\nContent")
                .await
                .unwrap();
            let link_path = skills_path.join("outside_skill");
            std::os::unix::fs::symlink(&outside_dir, &link_path).unwrap();

            let mut config = Config::default();
            config.workspace_dir = workspace_dir.clone();
            config.config_path = workspace_dir.join("config.toml");

            let blocked = handle_command(
                SkillCommands::Audit {
                    source: "outside_skill".to_string(),
                },
                &config,
            );
            assert!(
                blocked.is_err(),
                "audit should reject symlink target when trusted roots are not configured"
            );

            config.skills.trusted_skill_roots = vec![tmp.path().display().to_string()];
            let allowed = handle_command(
                SkillCommands::Audit {
                    source: "outside_skill".to_string(),
                },
                &config,
            );
            assert!(
                allowed.is_ok(),
                "audit should pass when symlink target is inside a trusted root"
            );
        }
    }
}
