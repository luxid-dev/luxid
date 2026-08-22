//! Turning one name the user typed into every name the generator needs.
//!
//! `luxid make:model User` has to produce `user.rs`, `users`, `UsersController`,
//! `users_controller.rs`, `StoreUser`, and a handful more. Getting these
//! consistent is most of what makes generated code feel like it belongs.

/// Every spelling of a model name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    /// `User`
    pub model: String,
    /// `user`
    pub snake: String,
    /// `users`
    pub plural: String,
    /// `UsersController`
    pub controller: String,
    /// `users_controller`
    pub controller_file: String,
    /// `StoreUser`
    pub store_request: String,
    /// `UpdateUser`
    pub update_request: String,
    /// `UserPolicy`
    pub policy: String,
    /// `UserFactory`
    pub factory: String,
    /// `UserSeeder`
    pub seeder: String,
}

impl Names {
    /// Accepts whatever the user typed — `User`, `user`, `user_profile`,
    /// `UserProfile` — and normalises from there.
    pub fn new(input: &str) -> Self {
        let snake = to_snake(input);
        let model = to_pascal(&snake);
        let plural = pluralize(&snake);

        Self {
            controller: format!("{}Controller", to_pascal(&plural)),
            controller_file: format!("{plural}_controller"),
            store_request: format!("Store{model}"),
            update_request: format!("Update{model}"),
            policy: format!("{model}Policy"),
            factory: format!("{model}Factory"),
            seeder: format!("{model}Seeder"),
            model,
            snake,
            plural,
        }
    }
}

/// `UserProfile` / `userProfile` / `user_profile` → `user_profile`.
pub fn to_snake(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 4);
    let mut previous_lower = false;

    for ch in input.chars() {
        if ch == '_' || ch == '-' || ch == ' ' {
            if !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
            previous_lower = false;
            continue;
        }

        if ch.is_uppercase() {
            if previous_lower && !out.is_empty() {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
            previous_lower = false;
        } else {
            out.push(ch);
            previous_lower = true;
        }
    }

    out
}

/// `user_profile` → `UserProfile`.
pub fn to_pascal(input: &str) -> String {
    input
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// The inverse of the singularisation `#[derive(Model)]` performs, and just as
/// deliberately simple. Irregular nouns are the user's to override.
pub fn pluralize(input: &str) -> String {
    let (head, last) = match input.rsplit_once('_') {
        Some((head, last)) => (Some(head), last),
        None => (None, input),
    };

    let plural = if last.is_empty() {
        String::new()
    } else if let Some(stem) = last.strip_suffix('y') {
        // `category` → `categories`, but `day` → `days`.
        if stem.chars().last().is_some_and(is_vowel) {
            format!("{last}s")
        } else {
            format!("{stem}ies")
        }
    } else if last.ends_with('s')
        || last.ends_with("sh")
        || last.ends_with("ch")
        || last.ends_with('x')
        || last.ends_with('z')
    {
        format!("{last}es")
    } else {
        format!("{last}s")
    };

    match head {
        Some(head) => format!("{head}_{plural}"),
        None => plural,
    }
}

fn is_vowel(ch: char) -> bool {
    matches!(ch, 'a' | 'e' | 'i' | 'o' | 'u')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_cases_every_spelling() {
        assert_eq!(to_snake("User"), "user");
        assert_eq!(to_snake("UserProfile"), "user_profile");
        assert_eq!(to_snake("user_profile"), "user_profile");
        assert_eq!(to_snake("userProfile"), "user_profile");
        assert_eq!(to_snake("user-profile"), "user_profile");
    }

    #[test]
    fn pascal_cases_from_snake() {
        assert_eq!(to_pascal("user"), "User");
        assert_eq!(to_pascal("user_profile"), "UserProfile");
    }

    #[test]
    fn pluralizes_the_common_shapes() {
        assert_eq!(pluralize("user"), "users");
        assert_eq!(pluralize("post"), "posts");
        assert_eq!(pluralize("category"), "categories");
        assert_eq!(
            pluralize("day"),
            "days",
            "a vowel before `y` just takes `s`"
        );
        assert_eq!(pluralize("address"), "addresses");
        assert_eq!(pluralize("branch"), "branches");
        assert_eq!(pluralize("box"), "boxes");
        assert_eq!(pluralize("user_profile"), "user_profiles");
    }

    #[test]
    fn round_trips_against_the_derive_singularizer() {
        // `#[derive(Model)]` singularises table names; these must agree or a
        // generated model reports the wrong name in its 404s.
        for word in [
            "users",
            "posts",
            "categories",
            "addresses",
            "branches",
            "user_profiles",
        ] {
            let singular = crate::naming::tests::singularize_like_derive(word);
            assert_eq!(pluralize(&singular), word, "round trip failed for {word}");
        }
    }

    /// Mirrors the rule in `luxid-macros`, so the two stay in step.
    fn singularize_like_derive(table: &str) -> String {
        let (head, last) = match table.rsplit_once('_') {
            Some((head, last)) => (Some(head), last),
            None => (None, table),
        };

        let singular = if let Some(stem) = last.strip_suffix("ies") {
            format!("{stem}y")
        } else if last.ends_with("sses") || last.ends_with("shes") || last.ends_with("ches") {
            last.trim_end_matches("es").to_owned()
        } else if last.ends_with("ss") {
            last.to_owned()
        } else if let Some(stem) = last.strip_suffix('s') {
            stem.to_owned()
        } else {
            last.to_owned()
        };

        match head {
            Some(head) => format!("{head}_{singular}"),
            None => singular,
        }
    }

    #[test]
    fn derives_every_name_from_one_input() {
        let names = Names::new("UserProfile");

        assert_eq!(names.model, "UserProfile");
        assert_eq!(names.snake, "user_profile");
        assert_eq!(names.plural, "user_profiles");
        assert_eq!(names.controller, "UserProfilesController");
        assert_eq!(names.controller_file, "user_profiles_controller");
        assert_eq!(names.store_request, "StoreUserProfile");
        assert_eq!(names.update_request, "UpdateUserProfile");
        assert_eq!(names.policy, "UserProfilePolicy");
    }

    #[test]
    fn normalises_however_the_user_typed_it() {
        assert_eq!(Names::new("user"), Names::new("User"));
        assert_eq!(Names::new("user_profile"), Names::new("UserProfile"));
    }
}
