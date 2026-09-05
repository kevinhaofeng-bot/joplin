import type { EditorControl, EditorProps } from './editor-types';
import type { OnCreateCodeEditor, RendererControl } from './editor-prosemirror-types';
export function createEditor(parent: HTMLElement, props: EditorProps, renderer: RendererControl, createCodeEditor: OnCreateCodeEditor): Promise<EditorControl>;
