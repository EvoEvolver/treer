"""Schema v2 and editable template contracts; no managed environment."""
import copy
import importlib.util
import unittest
from pathlib import Path
spec = importlib.util.spec_from_file_location('schema_policy', Path(__file__).resolve().parents[1] / 'policy.py')
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)

class SchemaTests(unittest.TestCase):
    def rule_document(self):
        return policy.template_document('team-isolation', {})['document']

    def test_templates_only_generate_valid_monitor_drafts(self):
        for template in policy.TEMPLATES:
            result = policy.template_document(template['id'], {'actor_path':'/team','target_path':'/'})
            self.assertEqual(result['mode'], 'monitor')
            self.assertTrue(result['warnings'])
            self.assertEqual(policy.validate_document(result['document']), result['document'])
        doc = self.rule_document()
        self.assertEqual(doc['defaults']['agent.prompt'], 'deny')
        self.assertEqual(doc['rules'][0]['conditions'], [{'op':'same_folder'}])
        self.assertEqual(doc['rules'][1]['conditions'], [{'op':'resource_below_subject_folder','strict':True}])

    def test_globs_are_component_bounded_and_canonical(self):
        for path in ('/', '/team/*', '/team/**', '/**', '/Team/中文/*'):
            self.assertEqual(policy.validate_path(path, True), path)
        for path in ('team', '/team/', '//team', '/a/../b', '/a/./b', '/a/**/b', '/a/**/**', '/re*', '/a/?', '/a/[x]', '/ team', '/team '):
            with self.subTest(path=path), self.assertRaises(policy.PolicyError): policy.validate_path(path, True)
        self.assertEqual(policy.validate_path('/literal/*'), '/literal/*')

    def test_unknown_fields_bindings_and_schema_one_rejected(self):
        baseline = self.rule_document()
        cases = []
        doc = copy.deepcopy(baseline);doc['schema_version'] = 1;cases.append(doc)
        doc = copy.deepcopy(baseline);doc['rules'][0]['subjects'][0]['tree']['path_glob'] = '/**';cases.append(doc)
        doc = copy.deepcopy(baseline);doc['rules'][0]['conditions'] = [{'op':'same_folder','strict':False}];cases.append(doc)
        doc = copy.deepcopy(baseline);doc['rules'][0]['bindings'] = {'a':{'var':'a'}};cases.append(doc)
        doc = copy.deepcopy(baseline);doc['rules'][0]['conditions'] = [{'op':'resource_below_folder','folder':{'var':'missing'}}];cases.append(doc)
        doc = copy.deepcopy(baseline);doc['rules'][0]['priority'] = True;cases.append(doc)
        doc = copy.deepcopy(baseline);doc['rules'][0]['conditions'] = [{'op':'same_folder'}] * 33;cases.append(doc)
        doc = copy.deepcopy(baseline);doc['rules'][0]['bindings'] = {str(i):{'path':'/'} for i in range(9)};cases.append(doc)
        for index,doc in enumerate(cases):
            with self.subTest(index=index), self.assertRaises(policy.PolicyError): policy.validate_document(doc)
        doc = copy.deepcopy(baseline)
        doc['rules'][0]['bindings'] = {'team':{'ref':'subject.tree.folder'}}
        doc['rules'][0]['conditions'] = [{'op':'resource_below_folder','folder':{'var':'team'},'strict':False}]
        self.assertEqual(policy.validate_document(doc), doc)

    def test_v2_cannot_use_local_v1_simulator(self):
        with self.assertRaises(policy.PolicyError) as caught:
            policy.evaluate_request({'subject':{},'action':'agent.prompt','resource':{}}, {'document':self.rule_document(),'mode':'monitor','revision':1})
        self.assertEqual(caught.exception.code, 'core_explain_required')

if __name__ == '__main__': unittest.main()
