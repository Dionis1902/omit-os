# Certificate authorities trusted by default

Encrypted connections made with `omit-tls` (the `fetch` program) accept
a server whose certificate leads to one of these authorities. The list
is Mozilla's — the authorities Mozilla's products trust for web sites —
as packaged by the `webpki-roots` crate, version 1.0.11; it changes when
that dependency is updated, and a test in `userspace/tls` fails if this
page was not regenerated then (`python3 tools/gen-authorities.py`).

The data is made available under the Community Data License Agreement –
Permissive – Version 2.0 (`LICENSES/CDLA-Permissive-2.0.txt`). Omit OS
does not choose or vet these authorities, and has no way yet to learn
that a certificate was revoked.

| Authority (Mozilla's label) | SHA-256 fingerprint of its certificate |
| --- | --- |
| AC RAIZ FNMT-RCM | `eb:c5:57:0c:29:01:8c:4d:67:b1:aa:12:7b:af:12:f7:03:b4:61:1e:bc:17:b7:da:b5:57:38:94:17:9b:93:fa` |
| AC RAIZ FNMT-RCM SERVIDORES SEGUROS | `55:41:53:b1:3d:2c:f9:dd:b7:53:bf:be:1a:4e:0a:e0:8d:0a:a4:18:70:58:fe:60:a2:b8:62:b2:e4:b8:7b:cb` |
| ACCVRAIZ1 | `9a:6e:c0:12:e1:a7:da:9d:be:34:19:4d:47:8a:d7:c0:db:18:22:fb:07:1d:f1:29:81:49:6e:d1:04:38:41:13` |
| Actalis Authentication Root CA | `55:92:60:84:ec:96:3a:64:b9:6e:2a:be:01:ce:0b:a8:6a:64:fb:fe:bc:c7:aa:b5:af:c1:55:b3:7f:d7:60:66` |
| Amazon Root CA 1 | `8e:cd:e6:88:4f:3d:87:b1:12:5b:a3:1a:c3:fc:b1:3d:70:16:de:7f:57:cc:90:4f:e1:cb:97:c6:ae:98:19:6e` |
| Amazon Root CA 2 | `1b:a5:b2:aa:8c:65:40:1a:82:96:01:18:f8:0b:ec:4f:62:30:4d:83:ce:c4:71:3a:19:c3:9c:01:1e:a4:6d:b4` |
| Amazon Root CA 3 | `18:ce:6c:fe:7b:f1:4e:60:b2:e3:47:b8:df:e8:68:cb:31:d0:2e:bb:3a:da:27:15:69:f5:03:43:b4:6d:b3:a4` |
| Amazon Root CA 4 | `e3:5d:28:41:9e:d0:20:25:cf:a6:90:38:cd:62:39:62:45:8d:a5:c6:95:fb:de:a3:c2:2b:0b:fb:25:89:70:92` |
| ANF Secure Server Root CA | `fb:8f:ec:75:91:69:b9:10:6b:1e:51:16:44:c6:18:c5:13:04:37:3f:6c:06:43:08:8d:8b:ef:fd:1b:99:75:99` |
| Atos TrustedRoot Root CA ECC TLS 2021 | `b2:fa:e5:3e:14:cc:d7:ab:92:12:06:47:01:ae:27:9c:1d:89:88:fa:cb:77:5f:a8:a0:08:91:4e:66:39:88:a8` |
| Atos TrustedRoot Root CA RSA TLS 2021 | `81:a9:08:8e:a5:9f:b3:64:c5:48:a6:f8:55:59:09:9b:6f:04:05:ef:bf:18:e5:32:4e:c9:f4:57:ba:00:11:2f` |
| Autoridad de Certificacion Firmaprofesional CIF A62634068 | `57:de:05:83:ef:d2:b2:6e:03:61:da:99:da:9d:f4:64:8d:ef:7e:e8:44:1c:3b:72:8a:fa:9b:cd:e0:f9:b2:6a` |
| BJCA Global Root CA1 | `f3:89:6f:88:fe:7c:0a:88:27:66:a7:fa:6a:d2:74:9f:b5:7a:7f:3e:98:fb:76:9c:1f:a7:b0:9c:2c:44:d5:ae` |
| BJCA Global Root CA2 | `57:4d:f6:93:1e:27:80:39:66:7b:72:0a:fd:c1:60:0f:c2:7e:b6:6d:d3:09:29:79:fb:73:85:64:87:21:28:82` |
| Buypass Class 2 Root CA | `9a:11:40:25:19:7c:5b:b9:5d:94:e6:3d:55:cd:43:79:08:47:b6:46:b2:3c:df:11:ad:a4:a0:0e:ff:15:fb:48` |
| Buypass Class 3 Root CA | `ed:f7:eb:bc:a2:7a:2a:38:4d:38:7b:7d:40:10:c6:66:e2:ed:b4:84:3e:4c:29:b4:ae:1d:5b:93:32:e6:b2:4d` |
| CA Disig Root R2 | `e2:3d:4a:03:6d:7b:70:e9:f5:95:b1:42:20:79:d2:b9:1e:df:bb:1f:b6:51:a0:63:3e:aa:8a:9d:c5:f8:07:03` |
| Certainly Root E1 | `b4:58:5f:22:e4:ac:75:6a:4e:86:12:a1:36:1c:5d:9d:03:1a:93:fd:84:fe:bb:77:8f:a3:06:8b:0f:c4:2d:c2` |
| Certainly Root R1 | `77:b8:2c:d8:64:4c:43:05:f7:ac:c5:cb:15:6b:45:67:50:04:03:3d:51:c6:0c:62:02:a8:e0:c3:34:67:d3:a0` |
| Certigna Root CA | `d4:8d:3d:23:ee:db:50:a4:59:e5:51:97:60:1c:27:77:4b:9d:7b:18:c9:4d:5a:05:95:11:a1:02:50:b9:31:68` |
| certSIGN ROOT CA G2 | `65:7c:fe:2f:a7:3f:aa:38:46:25:71:f3:32:a2:36:3a:46:fc:e7:02:09:51:71:07:02:cd:fb:b6:ee:da:33:05` |
| Certum EC-384 CA | `6b:32:80:85:62:53:18:aa:50:d1:73:c9:8d:8b:da:09:d5:7e:27:41:3d:11:4c:f7:87:a0:f5:d0:6c:03:0c:f6` |
| Certum Trusted Network CA | `5c:58:46:8d:55:f5:8e:49:7e:74:39:82:d2:b5:00:10:b6:d1:65:37:4a:cf:83:a7:d4:a3:2d:b7:68:c4:40:8e` |
| Certum Trusted Network CA 2 | `b6:76:f2:ed:da:e8:77:5c:d3:6c:b0:f6:3c:d1:d4:60:39:61:f4:9e:62:65:ba:01:3a:2f:03:07:b6:d0:b8:04` |
| Certum Trusted Root CA | `fe:76:96:57:38:55:77:3e:37:a9:5e:7a:d4:d9:cc:96:c3:01:57:c1:5d:31:76:5b:a9:b1:57:04:e1:ae:78:fd` |
| CFCA EV ROOT | `5c:c3:d7:8e:4e:1d:5e:45:54:7a:04:e6:87:3e:64:f9:0c:f9:53:6d:1c:cc:2e:f8:00:f3:55:c4:c5:fd:70:fd` |
| COMODO ECC Certification Authority | `17:93:92:7a:06:14:54:97:89:ad:ce:2f:8f:34:f7:f0:b6:6d:0f:3a:e3:a3:b8:4d:21:ec:15:db:ba:4f:ad:c7` |
| COMODO RSA Certification Authority | `52:f0:e1:c4:e5:8e:c6:29:29:1b:60:31:7f:07:46:71:b8:5d:7e:a8:0d:5b:07:27:34:63:53:4b:32:b4:02:34` |
| D-TRUST BR Root CA 1 2020 | `e5:9a:aa:81:60:09:c2:2b:ff:5b:25:ba:d3:7d:f3:06:f0:49:79:7c:1f:81:d8:5a:b0:89:e6:57:bd:8f:00:44` |
| D-TRUST BR Root CA 2 2023 | `05:52:e6:f8:3f:df:65:e8:fa:96:70:e6:66:df:28:a4:e2:13:40:b5:10:cb:e5:25:66:f9:7c:4f:b9:4b:2b:d1` |
| D-TRUST EV Root CA 1 2020 | `08:17:0d:1a:a3:64:53:90:1a:2f:95:92:45:e3:47:db:0c:8d:37:ab:aa:bc:56:b8:1a:a1:00:dc:95:89:70:db` |
| D-TRUST EV Root CA 2 2023 | `8e:82:21:b2:e7:d4:00:78:36:a1:67:2f:0d:cc:29:9c:33:bc:07:d3:16:f1:32:fa:1a:20:6d:58:71:50:f1:ce` |
| D-TRUST Root Class 3 CA 2 2009 | `49:e7:a4:42:ac:f0:ea:62:87:05:00:54:b5:25:64:b6:50:e4:f4:9e:42:e3:48:d6:aa:38:e0:39:e9:57:b1:c1` |
| D-TRUST Root Class 3 CA 2 EV 2009 | `ee:c5:49:6b:98:8c:e9:86:25:b9:34:09:2e:ec:29:08:be:d0:b0:f3:16:c2:d4:73:0c:84:ea:f1:f3:d3:48:81` |
| DigiCert Assured ID Root G2 | `7d:05:eb:b6:82:33:9f:8c:94:51:ee:09:4e:eb:fe:fa:79:53:a1:14:ed:b2:f4:49:49:45:2f:ab:7d:2f:c1:85` |
| DigiCert Assured ID Root G3 | `7e:37:cb:8b:4c:47:09:0c:ab:36:55:1b:a6:f4:5d:b8:40:68:0f:ba:16:6a:95:2d:b1:00:71:7f:43:05:3f:c2` |
| DigiCert Global Root G2 | `cb:3c:cb:b7:60:31:e5:e0:13:8f:8d:d3:9a:23:f9:de:47:ff:c3:5e:43:c1:14:4c:ea:27:d4:6a:5a:b1:cb:5f` |
| DigiCert Global Root G3 | `31:ad:66:48:f8:10:41:38:c7:38:f3:9e:a4:32:01:33:39:3e:3a:18:cc:02:29:6e:f9:7c:2a:c9:ef:67:31:d0` |
| DigiCert TLS ECC P384 Root G5 | `01:8e:13:f0:77:25:32:cf:80:9b:d1:b1:72:81:86:72:83:fc:48:c6:e1:3b:e9:c6:98:12:85:4a:49:0c:1b:05` |
| DigiCert TLS RSA4096 Root G5 | `37:1a:00:dc:05:33:b3:72:1a:7e:eb:40:e8:41:9e:70:79:9d:2b:0a:0f:2c:1d:80:69:31:65:f7:ce:c4:ad:75` |
| DigiCert Trusted Root G4 | `55:2f:7b:dc:f1:a7:af:9e:6c:e6:72:01:7f:4f:12:ab:f7:72:40:c7:8e:76:1a:c2:03:d1:d9:d2:0a:c8:99:88` |
| e-Szigno Root CA 2017 | `be:b0:0b:30:83:9b:9b:c3:2c:32:e4:44:79:05:95:06:41:f2:64:21:b1:5e:d0:89:19:8b:51:8a:e2:ea:1b:99` |
| e-Szigno TLS Root CA 2023 | `b4:91:41:50:2d:00:66:3d:74:0f:2e:7e:c3:40:c5:28:00:96:26:66:12:1a:36:d0:9c:f7:dd:2b:90:38:4f:b4` |
| emSign Root CA - G1 | `40:f6:af:03:46:a9:9a:a1:cd:1d:55:5a:4e:9c:ce:62:c7:f9:63:46:03:ee:40:66:15:83:3d:c8:c8:d0:03:67` |
| GDCA TrustAUTH R5 ROOT | `bf:ff:8f:d0:44:33:48:7d:6a:8a:a6:0c:1a:29:76:7a:9f:c2:bb:b0:5e:42:0f:71:3a:13:b9:92:89:1d:38:93` |
| GlobalSign | `17:9f:bc:14:8a:3d:d0:0f:d2:4e:a1:34:58:cc:43:bf:a7:f5:9c:81:82:d7:83:a5:13:f6:eb:ec:10:0c:89:24` |
| GlobalSign | `2c:ab:ea:fe:37:d0:6c:a2:2a:ba:73:91:c0:03:3d:25:98:29:52:c4:53:64:73:49:76:3a:3a:b5:ad:6c:cf:69` |
| GlobalSign | `b0:85:d7:0b:96:4f:19:1a:73:e4:af:0d:54:ae:7a:0e:07:aa:fd:af:9b:71:dd:08:62:13:8a:b7:32:5a:24:a2` |
| GlobalSign | `cb:b5:22:d7:b7:f1:27:ad:6a:01:13:86:5b:df:1c:d4:10:2e:7d:07:59:af:63:5a:7c:f4:72:0d:c9:63:c5:3b` |
| GlobalSign Root E46 | `cb:b9:c4:4d:84:b8:04:3e:10:50:ea:31:a6:9f:51:49:55:d7:bf:d2:e2:c6:b4:93:01:01:9a:d6:1d:9f:50:58` |
| GlobalSign Root R46 | `4f:a3:12:6d:8d:3a:11:d1:c4:85:5a:4f:80:7c:ba:d6:cf:91:9d:3a:5a:88:b0:3b:ea:2c:63:72:d9:3c:40:c9` |
| Go Daddy Root Certificate Authority - G2 | `45:14:0b:32:47:eb:9c:c8:c5:b4:f0:d7:b5:30:91:f7:32:92:08:9e:6e:5a:63:e2:74:9d:d3:ac:a9:19:8e:da` |
| GTS Root R1 | `d9:47:43:2a:bd:e7:b7:fa:90:fc:2e:6b:59:10:1b:12:80:e0:e1:c7:e4:e4:0f:a3:c6:88:7f:ff:57:a7:f4:cf` |
| GTS Root R3 | `34:d8:a7:3e:e2:08:d9:bc:db:0d:95:65:20:93:4b:4e:40:e6:94:82:59:6e:8b:6f:73:c8:42:6b:01:0a:6f:48` |
| GTS Root R4 | `34:9d:fa:40:58:c5:e2:63:12:3b:39:8a:e7:95:57:3c:4e:13:13:c8:3f:e6:8f:93:55:6c:d5:e8:03:1b:3c:7d` |
| HARICA TLS ECC Root CA 2021 | `3f:99:cc:47:4a:cf:ce:4d:fe:d5:87:94:66:5e:47:8d:15:47:73:9f:2e:78:0f:1b:b4:ca:9b:13:30:97:d4:01` |
| HARICA TLS RSA Root CA 2021 | `d9:5d:0e:8e:da:79:52:5b:f9:be:b1:1b:14:d2:10:0d:32:94:98:5f:0c:62:d9:fa:bd:9c:d9:99:ec:cb:7b:1d` |
| Hellenic Academic and Research Institutions ECC RootCA 2015 | `44:b5:45:aa:8a:25:e6:5a:73:ca:15:dc:27:fc:36:d2:4c:1c:b9:95:3a:06:65:39:b1:15:82:dc:48:7b:48:33` |
| Hellenic Academic and Research Institutions RootCA 2015 | `a0:40:92:9a:02:ce:53:b4:ac:f4:f2:ff:c6:98:1c:e4:49:6f:75:5e:6d:45:fe:0b:2a:69:2b:cd:52:52:3f:36` |
| HiPKI Root CA - G1 | `f0:15:ce:3c:c2:39:bf:ef:06:4b:e9:f1:d2:c4:17:e1:a0:26:4a:0a:94:be:1f:0c:8d:12:18:64:eb:69:49:cc` |
| Hongkong Post Root CA 3 | `5a:2f:c0:3f:0c:83:b0:90:bb:fa:40:60:4b:09:88:44:6c:76:36:18:3d:f9:84:6e:17:10:1a:44:7f:b8:ef:d6` |
| IdenTrust Commercial Root CA 1 | `5d:56:49:9b:e4:d2:e0:8b:cf:ca:d0:8a:3e:38:72:3d:50:50:3b:de:70:69:48:e4:2f:55:60:30:19:e5:28:ae` |
| IdenTrust Public Sector Root CA 1 | `30:d0:89:5a:9a:44:8a:26:20:91:63:55:22:d1:f5:20:10:b5:86:7a:ca:e1:2c:78:ef:95:8f:d4:f4:38:9f:2f` |
| ISRG Root X1 | `96:bc:ec:06:26:49:76:f3:74:60:77:9a:cf:28:c5:a7:cf:e8:a3:c0:aa:e1:1a:8f:fc:ee:05:c0:bd:df:08:c6` |
| ISRG Root X2 | `69:72:9b:8e:15:a8:6e:fc:17:7a:57:af:b7:17:1d:fc:64:ad:d2:8c:2f:ca:8c:f1:50:7e:34:45:3c:cb:14:70` |
| Izenpe.com | `25:30:cc:8e:98:32:15:02:ba:d9:6f:9b:1f:ba:1b:09:9e:2d:29:9e:0f:45:48:bb:91:4f:36:3b:c0:d4:53:1f` |
| Microsec e-Szigno Root CA 2009 | `3c:5f:81:fe:a5:fa:b8:2c:64:bf:a2:ea:ec:af:cd:e8:e0:77:fc:86:20:a7:ca:e5:37:16:3d:f3:6e:db:f3:78` |
| Microsoft ECC Root Certificate Authority 2017 | `35:8d:f3:9d:76:4a:f9:e1:b7:66:e9:c9:72:df:35:2e:e1:5c:fa:c2:27:af:6a:d1:d7:0e:8e:4a:6e:dc:ba:02` |
| Microsoft RSA Root Certificate Authority 2017 | `c7:41:f7:0f:4b:2a:8d:88:bf:2e:71:c1:41:22:ef:53:ef:10:eb:a0:cf:a5:e6:4c:fa:20:f4:18:85:30:73:e0` |
| NAVER Global Root Certification Authority | `88:f4:38:dc:f8:ff:d1:fa:8f:42:91:15:ff:e5:f8:2a:e1:e0:6e:0c:70:c3:75:fa:ad:71:7b:34:a4:9e:72:65` |
| NetLock Arany (Class Gold) Főtanúsítvány | `6c:61:da:c3:a2:de:f0:31:50:6b:e0:36:d2:a6:fe:40:19:94:fb:d1:3d:f9:c8:d4:66:59:92:74:c4:46:ec:98` |
| OISTE Server Root ECC G1 | `ee:c9:97:c0:c3:0f:21:6f:7e:3b:8b:30:7d:2b:ae:42:41:2d:75:3f:c8:21:9d:af:d1:52:0b:25:72:85:0f:49` |
| OISTE Server Root RSA G1 | `9a:e3:62:32:a5:18:9f:fd:db:35:3d:fd:26:52:0c:01:53:95:d2:27:77:da:c5:9d:b5:7b:98:c0:89:a6:51:e6` |
| OISTE WISeKey Global Root GB CA | `6b:9c:08:e8:6e:b0:f7:67:cf:ad:65:cd:98:b6:21:49:e5:49:4a:67:f5:84:5e:7b:d1:ed:01:9f:27:b8:6b:d6` |
| OISTE WISeKey Global Root GC CA | `85:60:f9:1c:36:24:da:ba:95:70:b5:fe:a0:db:e3:6f:f1:1a:83:23:be:94:86:85:4f:b3:f3:4a:55:71:19:8d` |
| QuoVadis Root CA 1 G3 | `8a:86:6f:d1:b2:76:b5:7e:57:8e:92:1c:65:82:8a:2b:ed:58:e9:f2:f2:88:05:41:34:b7:f1:f4:bf:c9:cc:74` |
| QuoVadis Root CA 2 G3 | `8f:e4:fb:0a:f9:3a:4d:0d:67:db:0b:eb:b2:3e:37:c7:1b:f3:25:dc:bc:dd:24:0e:a0:4d:af:58:b4:7e:18:40` |
| QuoVadis Root CA 3 G3 | `88:ef:81:de:20:2e:b0:18:45:2e:43:f8:64:72:5c:ea:5f:bd:1f:c2:d9:d2:05:73:07:09:c5:d8:b8:69:0f:46` |
| SECOM TLS ECC Root CA 2024 | `6a:b2:ab:75:f5:1c:b4:f4:f0:15:62:03:fb:f6:f6:46:23:2f:51:4b:e0:59:f6:28:33:30:8b:82:b4:d7:2d:b1` |
| SECOM TLS RSA Root CA 2024 | `14:35:f2:25:c5:d2:52:d7:a2:19:48:cc:3c:e6:2a:ec:fa:88:00:1e:3d:d7:2d:1c:c3:55:51:00:eb:37:2f:93` |
| Sectigo Public Server Authentication Root E46 | `c9:0f:26:f0:fb:1b:40:18:b2:22:27:51:9b:5c:a2:b5:3e:2c:a5:b3:be:5c:f1:8e:fe:1b:ef:47:38:0c:53:83` |
| Sectigo Public Server Authentication Root R46 | `7b:b6:47:a6:2a:ee:ac:88:bf:25:7a:a5:22:d0:1f:fe:a3:95:e0:ab:45:c7:3f:93:f6:56:54:ec:38:f2:5a:06` |
| SecureSign Root CA14 | `4b:00:9c:10:34:49:4f:9a:b5:6b:ba:3b:a1:d6:27:31:fc:4d:20:d8:95:5a:dc:ec:10:a9:25:60:72:61:e3:38` |
| SecureSign Root CA15 | `e7:78:f0:f0:95:fe:84:37:29:cd:1a:00:82:17:9e:53:14:a9:c2:91:44:28:05:e1:fb:1d:8f:b6:b8:88:6c:3a` |
| Security Communication ECC RootCA1 | `e7:4f:bd:a5:5b:d5:64:c4:73:a3:6b:44:1a:a7:99:c8:a6:8e:07:74:40:e8:28:8b:9f:a1:e5:0e:4b:ba:ca:11` |
| Security Communication RootCA2 | `51:3b:2c:ec:b8:10:d4:cd:e5:dd:85:39:1a:df:c6:c2:dd:60:d8:7b:b7:36:d2:b5:21:48:4a:a4:7a:0e:be:f6` |
| SSL.com EV Root Certification Authority ECC | `22:a2:c1:f7:bd:ed:70:4c:c1:e7:01:b5:f4:08:c3:10:88:0f:e9:56:b5:de:2a:4a:44:f9:9c:87:3a:25:a7:c8` |
| SSL.com EV Root Certification Authority RSA R2 | `2e:7b:f1:6c:c2:24:85:a7:bb:e2:aa:86:96:75:07:61:b0:ae:39:be:3b:2f:e9:d0:cc:6d:4e:f7:34:91:42:5c` |
| SSL.com Root Certification Authority ECC | `34:17:bb:06:cc:60:07:da:1b:96:1c:92:0b:8a:b4:ce:3f:ad:82:0e:4a:a3:0b:9a:cb:c4:a7:4e:bd:ce:bc:65` |
| SSL.com Root Certification Authority RSA | `85:66:6a:56:2e:e0:be:5c:e9:25:c1:d8:89:0a:6f:76:a8:7e:c1:6d:4d:7d:5f:29:ea:74:19:cf:20:12:3b:69` |
| SSL.com TLS ECC Root CA 2022 | `c3:2f:fd:9f:46:f9:36:d1:6c:36:73:99:09:59:43:4b:9a:d6:0a:af:bb:9e:7c:f3:36:54:f1:44:cc:1b:a1:43` |
| SSL.com TLS RSA Root CA 2022 | `8f:af:7d:2e:2c:b4:70:9b:b8:e0:b3:36:66:bf:75:a5:dd:45:b5:de:48:0f:8e:a8:d4:bf:e6:be:bc:17:f2:ed` |
| Starfield Root Certificate Authority - G2 | `2c:e1:cb:0b:f9:d2:f9:e1:02:99:3f:be:21:51:52:c3:b2:dd:0c:ab:de:1c:68:e5:31:9b:83:91:54:db:b7:f5` |
| Starfield Services Root Certificate Authority - G2 | `56:8d:69:05:a2:c8:87:08:a4:b3:02:51:90:ed:cf:ed:b1:97:4a:60:6a:13:c6:e5:29:0f:cb:2a:e6:3e:da:b5` |
| SwissSign RSA TLS Root CA 2022 - 1 | `19:31:44:f4:31:e0:fd:db:74:07:17:d4:de:92:6a:57:11:33:88:4b:43:60:d3:0e:27:29:13:cb:e6:60:ce:41` |
| SZAFIR ROOT CA2 | `a1:33:9d:33:28:1a:0b:56:e5:57:d3:d3:2b:1c:e7:f9:36:7e:b0:94:bd:5f:a7:2a:7e:50:04:c8:de:d7:ca:fe` |
| T-TeleSec GlobalRoot Class 2 | `91:e2:f5:78:8d:58:10:eb:a7:ba:58:73:7d:e1:54:8a:8e:ca:cd:01:45:98:bc:0b:14:3e:04:1b:17:05:25:52` |
| T-TeleSec GlobalRoot Class 3 | `fd:73:da:d3:1c:64:4f:f1:b4:3b:ef:0c:cd:da:96:71:0b:9c:d9:87:5e:ca:7e:31:70:7a:f3:e9:6d:52:2b:bd` |
| Telekom Security TLS ECC Root 2020 | `57:8a:f4:de:d0:85:3f:4e:59:98:db:4a:ea:f9:cb:ea:8d:94:5f:60:b6:20:a3:8d:1a:3c:13:b2:bc:7b:a8:e1` |
| Telekom Security TLS RSA Root 2023 | `ef:c6:5c:ad:bb:59:ad:b6:ef:e8:4d:a2:23:11:b3:56:24:b7:1b:3b:1e:a0:da:8b:66:55:17:4e:c8:97:86:46` |
| Telia EC TLS Root CA v3 | `09:8e:08:a9:1d:bb:f7:74:78:b9:6c:ce:b8:9b:14:13:a5:da:37:b7:c8:62:60:6a:95:5d:eb:07:17:9f:43:26` |
| Telia Root CA v2 | `24:2b:69:74:2f:cb:1e:5b:2a:bf:98:89:8b:94:57:21:87:54:4e:5b:4d:99:11:78:65:73:62:1f:6a:74:b8:2c` |
| Telia RSA TLS Root CA v3 | `d1:3d:b1:29:4c:45:eb:c6:fc:86:c6:bb:f6:9f:a2:9b:df:e6:92:df:f7:c7:13:c2:43:c7:a9:56:c6:a2:28:4c` |
| TrustAsia Global Root CA G3 | `e0:d3:22:6a:eb:11:63:c2:e4:8f:f9:be:3b:50:b4:c6:43:1b:e7:bb:1e:ac:c5:c3:6b:5d:5e:c5:09:03:9a:08` |
| TrustAsia Global Root CA G4 | `be:4b:56:cb:50:56:c0:13:6a:52:6d:f4:44:50:8d:aa:36:a0:b5:4f:42:e4:ac:38:f7:2a:f4:70:e4:79:65:4c` |
| TrustAsia TLS ECC Root CA | `c0:07:6b:9e:f0:53:1f:b1:a6:56:d6:7c:4e:be:97:cd:5d:ba:a4:1e:f4:45:98:ac:c2:48:98:78:c9:2d:87:11` |
| TrustAsia TLS RSA Root CA | `06:c0:8d:7d:af:d8:76:97:1e:b1:12:4f:e6:7f:84:7e:c0:c7:a1:58:d3:ea:53:cb:e9:40:e2:ea:97:91:f4:c3` |
| TUBITAK Kamu SM SSL Kok Sertifikasi - Surum 1 | `46:ed:c3:68:90:46:d5:3a:45:3f:b3:10:4a:b8:0d:ca:ec:65:8b:26:60:ea:16:29:dd:7e:86:79:90:64:87:16` |
| TunTrust Root CA | `2e:44:10:2a:b5:8c:b8:54:19:45:1c:8e:19:d9:ac:f3:66:2c:af:bc:61:4b:6a:53:96:0a:30:f7:d0:e2:eb:41` |
| TWCA CYBER Root CA | `3f:63:bb:28:14:be:17:4e:c8:b6:43:9c:f0:8d:6d:56:f0:b7:c4:05:88:3a:56:48:a3:34:42:4d:6b:3e:c5:58` |
| TWCA Global Root CA | `59:76:90:07:f7:68:5d:0f:cd:50:87:2f:9f:95:d5:75:5a:5b:2b:45:7d:81:f3:69:2b:61:0a:98:67:2f:0e:1b` |
| TWCA Root Certification Authority | `bf:d8:8f:e1:10:1c:41:ae:3e:80:1b:f8:be:56:35:0e:e9:ba:d1:a6:b9:bd:51:5e:dc:5c:6d:5b:87:11:ac:44` |
| UCA Extended Validation Root | `d4:3a:f9:b3:54:73:75:5c:96:84:fc:06:d7:d8:cb:70:ee:5c:28:e7:73:fb:29:4e:b4:1e:e7:17:22:92:4d:24` |
| UCA Global G2 Root | `9b:ea:11:c9:76:fe:01:47:64:c1:be:56:a6:f9:14:b5:a5:60:31:7a:bd:99:88:39:33:82:e5:16:1a:a0:49:3c` |
| USERTrust ECC Certification Authority | `4f:f4:60:d5:4b:9c:86:da:bf:bc:fc:57:12:e0:40:0d:2b:ed:3f:bc:4d:4f:bd:aa:86:e0:6a:dc:d2:a9:ad:7a` |
| USERTrust RSA Certification Authority | `e7:93:c9:b0:2f:d8:aa:13:e2:1c:31:22:8a:cc:b0:81:19:64:3b:74:9c:89:89:64:b1:74:6d:46:c3:d4:cb:d2` |
| vTrus ECC Root CA | `30:fb:ba:2c:32:23:8e:2a:98:54:7a:f9:79:31:e5:50:42:8b:9b:3f:1c:8e:eb:66:33:dc:fa:86:c5:b2:7d:d3` |
| vTrus Root CA | `8a:71:de:65:59:33:6f:42:6c:26:e5:38:80:d0:0d:88:a1:8d:a4:c6:a9:1f:0d:cb:61:94:e2:06:c5:c9:63:87` |
