import ast
import pathlib
import re
import unittest

U = chr(95) + chr(95)  # dunder prefix/suffix
HERE = pathlib.Path(globals()[U + 'file' + U]).resolve()
WORKER = HERE.parents[1] / 'funasr_worker.py'
BAR = chr(124)


def tag(name):
    return '<' + BAR + name + BAR + '>'


def tags(*names):
    return ''.join(tag(n) for n in names)


def load(names):
    # Importing the worker pulls in FunASR, so compile just the named nodes.
    tree = ast.parse(WORKER.read_text(encoding='utf-8'))
    nodes = [
        n
        for n in tree.body
        if (isinstance(n, ast.FunctionDef) and n.name in names)
        or (isinstance(n, ast.Assign) and any(getattr(t, 'id', None) in names for t in n.targets))
    ]
    namespace = {'re': re}
    exec(compile(ast.Module(body=nodes, type_ignores=[]), str(WORKER), 'exec'), namespace)
    return namespace


clean_text = load({'SENSEVOICE_TAG', 'clean_text'})['clean_text']


class SenseVoiceTagTest(unittest.TestCase):
    def test_strips_language_emotion_event_and_itn_tags(self):
        text = '\u3082\u3063\u3068'
        self.assertEqual(clean_text(tags('ja', 'SURPRISED', 'Speech', 'woitn') + text), text)

    def test_event_tags_leave_no_emoji(self):
        text = '\u3075\u3075'
        self.assertEqual(clean_text(tags('ja', 'HAPPY', 'Laughter', 'withitn') + text), text)

    def test_tag_only_span_is_empty(self):
        self.assertEqual(clean_text(tags('ja', 'NEUTRAL', 'BGM', 'woitn')), '')

    def test_plain_text_is_kept(self):
        self.assertEqual(clean_text(' a' + BAR + 'b <c> '), 'a' + BAR + 'b <c>')


if globals()[U + 'name' + U] == U + 'main' + U:
    unittest.main()
