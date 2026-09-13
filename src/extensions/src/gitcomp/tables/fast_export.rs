use crate::gitcomp::*;

/// `options[]` (builtin/fast-export.c:1323-1367).
pub(super) const FAST_EXPORT_OPTIONS: &[Opt] = &[
    OPT_INTEGER("progress"),
    OPT_CALLBACK("signed-tags"),
    OPT_CALLBACK("signed-commits"),
    OPT_CALLBACK("tag-of-filtered-object"),
    OPT_CALLBACK("reencode"),
    OPT_STRING("export-marks"),
    OPT_STRING("import-marks"),
    OPT_STRING("import-marks-if-exists"),
    OPT_BOOL("fake-missing-tagger"),
    OPT_BOOL("full-tree"),
    OPT_BOOL("use-done-feature"),
    OPT_BOOL("no-data"),
    OPT_STRING_LIST("refspec"),
    OPT_BOOL("anonymize"),
    OPT_CALLBACK_F("anonymize-map", PARSE_OPT_NONEG),
    OPT_BOOL("reference-excluded-parents"),
    OPT_BOOL("show-original-ids"),
    OPT_BOOL("mark-tags"),
];
