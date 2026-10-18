//! Flags read from release names.

use torrential_core::Category;

/// Tokens no ordinary title contains.
const CAM_ANY_CASE: &[&str] = &["hdcam", "camrip", "hdcamrip", "telesync", "telecine", "hdts", "hdtc"];
/// Short tokens that collide with ordinary words ("Cam Newton", "TS Eliot") and file
/// types (MPEG-TS). They count only in capitals and after the release year, where
/// release groups put the source.
const CAM_AFTER_YEAR: &[&str] = &["CAM", "TS"];

/// Brands and terms that do not occur in ordinary titles. Bare "xxx" and "sex" are
/// left out because they collide with "xXx (2002)" and "Sex Education"; a source's
/// adult category catches those listings instead.
const ADULT: &[&str] = &[
    "bangbros",
    "brazzers",
    "camwhores",
    "chaturbate",
    "digitalplayground",
    "evilangel",
    "fansly",
    "hentai",
    "javhd",
    "manyvids",
    "myfreecams",
    "naughtyamerica",
    "onlyfans",
    "porn",
    "pornhub",
    "realitykings",
    "xnxx",
    "xvideos",
];

pub(crate) fn is_cam(name: &str, category: Option<Category>) -> bool {
    if category.is_some_and(|c| c != Category::Video) {
        return false;
    }
    let tokens: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let mut after_year = false;
    let mut previous = "";
    for token in tokens {
        if CAM_ANY_CASE.contains(&token.to_ascii_lowercase().as_str()) {
            return true;
        }
        if after_year && CAM_AFTER_YEAR.contains(&token) && !previous.eq_ignore_ascii_case("mpeg") {
            return true;
        }
        after_year |= token.len() == 4 && token.parse::<u16>().is_ok_and(|y| (1900..=2099).contains(&y));
        previous = token;
    }
    false
}

pub(crate) fn is_adult(name: &str) -> bool {
    name.split(|c: char| !c.is_alphanumeric())
        .any(|t| ADULT.contains(&t.to_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spots_camera_releases() {
        assert!(is_cam("Oppenheimer 2023 HDCAM x264", None));
        assert!(is_cam("Oppenheimer.2023.HDTS.720p", Some(Category::Video)));
        assert!(is_cam("Some Movie 2024 TS 1080p", None));
        assert!(is_cam("Some Movie (2024) CAM", None));
        assert!(is_cam("Movie 2023 TELESYNC", None));
    }

    #[test]
    fn leaves_lookalikes_alone() {
        assert!(!is_cam("Cam Newton Highlights 2019", None));
        assert!(!is_cam("Dash CAM Compilation 2019", None));
        assert!(!is_cam("Concert 2021 MPEG-TS 1080p", None));
        assert!(!is_cam("Learn TS 2024 Edition", Some(Category::Book)));
        assert!(!is_cam("Webcam Recorder 2024 ts", None));
        assert!(!is_cam("ubuntu-24.04.5.1-desktop-amd64.iso", Some(Category::Software)));
    }

    #[test]
    fn spots_adult_names_without_eating_real_titles() {
        assert!(is_adult("Model OnlyFans Leak 2024"));
        assert!(is_adult("Brazzers.2024.1080p"));
        assert!(is_adult("Amateur Porn 2024"));
        assert!(!is_adult("xXx (2002) 1080p BluRay"));
        assert!(!is_adult("Sex Education S04 1080p"));
        assert!(!is_adult("Sussex Documentary 2021"));
        assert!(!is_adult("Portlandia S01 1080p"));
        assert!(!is_adult("Ubuntu 24.04 LTS"));
    }
}
