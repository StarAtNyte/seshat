// Piper voice packs for languages Kokoro doesn't cover, one voice per
// language, as published by sherpa-onnx (GitHub release "tts-models").
// Shared by the helper (download + hash check) and Seshat (the list in
// Settings) via include!, so the two can never disagree.
//
// (language code, language name, pack name, archive bytes, archive SHA-256)
// Archive URL: https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-piper-<pack>.tar.bz2
pub const PIPER_VOICES: &[(&str, &str, &str, u64, &str)] = &[
    ("ar", "Arabic", "ar_JO-kareem-medium", 67177830, "9ebbcea30e0fbd588f7b2cb45ee897d6aeb1bf5791cbc037a7b5a3f641e3dbce"),
    ("ca", "Catalan", "ca_ES-upc_ona-medium", 67223188, "b6c50f22e13cfc72ce51c496d22b987adf6704047f6d342968197b4b0b3127a5"),
    ("cs", "Czech", "cs_CZ-jirka-medium", 67212775, "321aafd83dd89b3d5b2c36c5898e0710d4f472f3c191cb7f73901a89b7f8539f"),
    ("cy", "Welsh", "cy_GB-gwryw_gogleddol-medium", 67230234, "f31f90afc4f28e9a768309f0b1c87272ef178936b32dd181aa1aa3a0eeddd187"),
    ("da", "Danish", "da_DK-talesyntese-medium", 67185257, "f2e2cbcdb2b21e76f93635b35105d07b37997ace434d7289d04b689a97c19bd0"),
    ("de", "German", "de_DE-thorsten-medium", 67214254, "50487d9c95fdf2191f31d2588569381063ba1591dcd4c7d4bdd30f12b2191714"),
    ("el", "Greek", "el_GR-rapunzelina-low", 67105521, "bb868070a9ba0d7220b3d7c9291a998c2f60accdb73525bd63b8751b86dd4839"),
    ("es", "Spanish", "es_ES-davefx-medium", 67184952, "a3f6beb54a9cb893279f72978a22f807a4d9fc9c7848157b524d5cc7b7f58b22"),
    ("fa", "Persian", "fa_IR-amir-medium", 67180373, "46af65c1cdd86ec300db3998f2bf31a68d9e38da930c0655c1ff6f610e84f864"),
    ("fi", "Finnish", "fi_FI-harri-medium", 67205198, "a7fd608e73983c366a5e0bcd13382e6529a6749f67efc406b5a7f9021e3a8b1d"),
    ("fr", "French", "fr_FR-siwis-medium", 67207459, "375909aa30842b3a4efa10b1beb1d761af792960ae6873b4d53889f96c66195b"),
    ("hi", "Hindi", "hi_IN-pratham-medium", 67238438, "2084d321e1d2752f2b64ed3012ba27751df01a80da46f52920098cdcb7e35648"),
    ("hu", "Hungarian", "hu_HU-anna-medium", 67167701, "c67b13e57b71eca19098fe499afc35ebce8f63a0b524017c786af2025821216a"),
    ("id", "Indonesian", "id_ID-news_tts-medium", 67243848, "634b0169fe41c368e9fafe128f30647f9e0301e852ce503cd78146db524f1f12"),
    ("is", "Icelandic", "is_IS-bui-medium", 67271654, "404a4de4b2a49dafb5a9f67ccf5366ab19d97e146aab602d348d8ba39dfb89d9"),
    ("it", "Italian", "it_IT-paola-medium", 67221173, "7541f75778afa164e44e34baaef63befad7698595df26a95ca944b63ef1a16b4"),
    ("ka", "Georgian", "ka_GE-natia-medium", 67209457, "18e34c601f4be13e5132ec772e2cbcfa47ca6a4aaaf995dd5baefe014f5a0256"),
    ("kk", "Kazakh", "kk_KZ-issai-high", 129278805, "6c59955f7f5e3fc50b130ced59a430eb4ef62d69155af4ce836db0dde4a42682"),
    ("lb", "Luxembourgish", "lb_LU-marylux-medium", 67184319, "f11117fc121db0d5c4dc46e07eac7b1d8ea719b04f0b381836a23050310ea5a4"),
    ("lv", "Latvian", "lv_LV-aivars-medium", 67196173, "a0df58c9924761e201ef75d82473b90bde32c5372267e9b4c628fdd267cf2508"),
    ("ml", "Malayalam", "ml_IN-meera-medium", 67195010, "c8d870441e60870b7e8781c26eb22104aff51587b63bee94aba5e5f6c9fbb71f"),
    ("ne", "Nepali", "ne_NP-google-medium", 80242801, "73c1e093451867779ec2b55cc3666de2db058019a2e7760e8c8f294a42caca2a"),
    ("nl", "Dutch", "nl_NL-ronnie-medium", 67194164, "d2cc688abf46054eba1d47b3b1406c742ccf66732cd47998a3ee727c1b96c09f"),
    ("no", "Norwegian", "no_NO-talesyntese-medium", 67194829, "dcb7c36b7222e290cca1fd4cdecb474e7fd651dc5b0d3d42c33d416a8e62018b"),
    ("pl", "Polish", "pl_PL-gosia-medium", 67211182, "75bd34dcbdc4dd98d763954756b4b34b4208100497c836381542e4d73dcefa9c"),
    ("pt", "Portuguese (Brazil)", "pt_BR-faber-medium", 67183065, "7add3f923ad6bc25ca8a192805fd1a64d1b3893e4611c4a9719545a825039a83"),
    ("ro", "Romanian", "ro_RO-mihai-medium", 67182137, "cfbee3cdc25cf063339f69e189f4d93cc3be6610d267567537323ced3cf87f4d"),
    ("ru", "Russian", "ru_RU-irina-medium", 67153308, "1fc0f54e5e084fe287c07909f2f6e0ba6d857864cf800e3ab80286a4e8233008"),
    ("sk", "Slovak", "sk_SK-lili-medium", 67183092, "a00dfbca5d34733862a59d9e309f4e94a92fccc9337c2b124de9a241b604fc0d"),
    ("sl", "Slovenian", "sl_SI-artur-medium", 67025668, "df805f31c4513864edebf7e287cf0b54d72788fc661ed73f88dc4e5aabc5efd5"),
    ("sr", "Serbian", "sr_RS-serbski_institut-medium", 80298557, "b8c100e5ff885d11ff96ad92a953f4b82e1cd649f9ac60a77ae4553d37507b2b"),
    ("sv", "Swedish", "sv_SE-nst-medium", 67144884, "5aed0f5ab75135fc3ba6a4f3567e171d72df753305b08a01ca42f79411587f67"),
    ("sw", "Swahili", "sw_CD-lanfrica-medium", 67217230, "93196704a0ae0c984806059480775d69dcc2419d0d545cddd0903d244dc66a72"),
    ("tr", "Turkish", "tr_TR-dfki-medium", 67201221, "b63babaefcdd63202c7079aba25b72a2013825ad5895e2aeff03d1ae5a5b3bbf"),
    ("uk", "Ukrainian", "uk_UA-ukrainian_tts-medium", 80355083, "92be6ff21ebb1cbcc0c71121a6437db8d060cee5785287aaa6a19a2548f59c03"),
    ("ur", "Urdu", "ur_PK-fasih-medium", 67227718, "de64e03109f3c896b7cf910db6d9ffd2703e3d665580d9a19bedbf3e0760d0e4"),
    ("vi", "Vietnamese", "vi_VN-vais1000-medium", 67154040, "fa1367710767d36ed5cf13b4a449e20c35ffd12791c2e47c2e64142bfa55551a"),
    ("zh", "Chinese", "zh_CN-xiao_ya-medium", 60462944, "9396a3dffbb95b037acaa18094500f58d0db9a7c4f2689554e2539717cf0db65"),
];
