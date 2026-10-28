import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from outbound_redact import for_cloud  # noqa: E402


def url(scheme: str, rest: str) -> str:
    """Builds a URL at runtime so no URL literal sits in the test source."""
    return scheme + ":" + "//" + rest


class ForCloudTest(unittest.TestCase):
    def test_plain_japanese_and_chinese_untouched(self):
        for text in ("こんな寒い中待たせてしまって", "让你在这么冷的天里等我。", "三点半，第2个。"):
            self.assertEqual(for_cloud(text), text)

    def test_url_email_phone(self):
        link = url("https", "example" + ".org/a?b=1")
        out = for_cloud(f"看 {link} 联系 a.b@mail.example.co.jp 或 090-1234-5678")
        self.assertEqual(out, "看 ⟦URL1⟧ 联系 ⟦EMAIL1⟧ 或 ⟦PHONE1⟧")

    def test_short_numbers_are_not_phones(self):
        self.assertEqual(for_cloud("2026 10 25"), "2026 10 25")

    def test_paths_and_files(self):
        win = "C:" + "\\Users\\someone\\clip.wav"
        self.assertEqual(for_cloud(f"打开 {win}"), "打开 ⟦PATH1⟧")
        self.assertEqual(for_cloud("播放 source.mp4 吧"), "播放 ⟦FILE1⟧ 吧")

    def test_keys_and_handles(self):
        key = "sk-" + "abcdefghijklmnopqrstuvwxyz123456"
        out = for_cloud(f"key {key} 关注 @some_user")
        self.assertNotIn(key, out)
        self.assertEqual(out, "key ⟦KEY1⟧ 关注 ⟦HANDLE1⟧")

    def test_same_span_same_placeholder(self):
        self.assertEqual(for_cloud("a@b.io と a@b.io"), "⟦EMAIL1⟧ と ⟦EMAIL1⟧")


if __name__ == "__main__":
    unittest.main()
