import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class CliTests(unittest.TestCase):
    def call(self, *args):
        return subprocess.run([sys.executable, '-m', 'chainkit_indexer', *args], capture_output=True, text=True, check=True)

    def test_local_project_lifecycle_and_machine_schema(self):
        with tempfile.TemporaryDirectory() as td:
            db = str(Path(td)/'cli.sqlite')
            created = json.loads(self.call('--database', db, 'create-project', 'example', '--name', 'Example').stdout)
            self.assertEqual(set(created['tokens']), {'read', 'manage'})
            listed = json.loads(self.call('--database', db, 'list-projects').stdout)
            self.assertEqual(len(listed), 1)
            self.assertNotIn('tokens', listed[0])
            rotated = json.loads(self.call('--database', db, 'rotate-key', 'example', '--role', 'read').stdout)
            self.assertNotEqual(rotated['token'], created['tokens']['read'])
            paused = json.loads(self.call('--database', db, 'pause-project', 'example').stdout)
            self.assertEqual(paused['paused'], 1)
            resumed = json.loads(self.call('--database', db, 'pause-project', 'example', '--resume').stdout)
            self.assertEqual(resumed['paused'], 0)
            spec = json.loads(self.call('--database', str(Path(td)/'unused.sqlite'), 'openapi').stdout)
            self.assertFalse((Path(td)/'unused.sqlite').exists())
            self.assertIn('Coverage', spec['components']['schemas'])
