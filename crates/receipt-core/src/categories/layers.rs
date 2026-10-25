//! Build category rules from ordered configurations and account overrides.
//!
//! TOML parsing lives in `rules::corpus`; this module consumes its typed inputs
//! and applies layer priorities, exact-only keywords, brands and account policy.

use std::collections::{HashMap, HashSet};

use super::{expand_tag_paths, BuildClassifierConfig, CategoryRule, CategoryRuleLayers, TagNode};

pub fn build_rule_layers(
    default_account_mapping: HashMap<String, String>,
    classifier_configs: Vec<BuildClassifierConfig>,
    account_configs: Vec<HashMap<String, String>>,
    tag_vocabulary: Vec<TagNode>,
) -> CategoryRuleLayers {
    let mut account_mapping = default_account_mapping;
    for config in account_configs {
        for (key, value) in config {
            let key = key.trim();
            let value = value.trim();
            if !key.is_empty() && !value.is_empty() {
                account_mapping.insert(key.to_string(), value.to_string());
            }
        }
    }

    let mut exact_only_keywords = HashSet::new();
    let mut brands: Vec<String> = Vec::new();
    let mut rules = Vec::new();

    for (idx, config) in classifier_configs.into_iter().enumerate() {
        let layer_priority = ((idx + 1) as i32) * 100;
        for brand in config.brands {
            let cleaned = brand.trim().to_ascii_uppercase();
            if !cleaned.is_empty() && !brands.contains(&cleaned) {
                brands.push(cleaned);
            }
        }
        for keyword in config.exact_only_keywords {
            let cleaned = keyword.trim();
            if !cleaned.is_empty() {
                exact_only_keywords.insert(cleaned.to_string());
            }
        }

        for rule in config.rules {
            if rule.keywords.is_empty() || rule.tag_paths.is_empty() {
                continue;
            }
            // Exact lookup, no walk-up: `grocery/dairy/milk` has no account
            // entry, so the milk rule adds a tag and claims nothing — which is
            // how the former tag-only rules keep behaving as they always have.
            let category = rule
                .tag_paths
                .iter()
                .find(|path| account_mapping.contains_key(*path))
                .cloned();
            let tags = expand_tag_paths(&rule.tag_paths);
            if rule.exact_only {
                for keyword in &rule.keywords {
                    exact_only_keywords.insert(keyword.clone());
                }
            }
            rules.push(CategoryRule {
                id: rule.id,
                remove_tags: rule.remove_tags,
                disables: rule.disables,
                keywords: rule.keywords,
                tag_paths: rule.tag_paths,
                category,
                tags,
                priority: rule.priority + layer_priority,
                exact_only: rule.exact_only,
                layer: idx,
                brand: rule.brand,
            });
        }
    }

    CategoryRuleLayers {
        rules,
        exact_only_keywords,
        brands,
        account_mapping,
        tag_vocabulary,
    }
}
