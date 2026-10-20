import js from '@eslint/js'
import prettier from 'eslint-config-prettier'
import { defineConfig, globalIgnores } from 'eslint/config'
import globals from 'globals'
import tseslint from 'typescript-eslint'

export default defineConfig(
  globalIgnores([
    '**/dist/',
    'target/',
    '**/test-results/',
    '**/playwright-report/',
    '**/.rstack/',
  ]),
  js.configs.recommended,
  tseslint.configs.strictTypeChecked,
  tseslint.configs.stylisticTypeChecked,
  {
    languageOptions: {
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
    },
    rules: {
      // Pairs with `verbatimModuleSyntax`: type-only imports must say so.
      '@typescript-eslint/consistent-type-imports': 'error',
      '@typescript-eslint/no-import-type-side-effects': 'error',
      // `T[]` for simple element types, `Array<T>` / `ReadonlyArray<T>` for anything else.
      '@typescript-eslint/array-type': ['error', { default: 'array-simple' }],
      // Numbers in templates (ports, statuses) are unambiguous; everything else stays strict.
      '@typescript-eslint/restrict-template-expressions': ['error', { allowNumber: true }],
    },
  },
  {
    // Plain JS (Node scripts, tool configs) is linted without type information.
    files: ['**/*.mjs', '**/*.js'],
    extends: [tseslint.configs.disableTypeChecked],
    languageOptions: { globals: globals.node },
  },
  prettier,
)
