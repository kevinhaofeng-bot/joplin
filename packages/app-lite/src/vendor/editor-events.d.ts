export enum EditorEventType { Change = 0, Remove = 8 }
export interface ChangeEvent { kind: EditorEventType.Change; value: string }
export interface RemoveEvent { kind: EditorEventType.Remove }
export type EditorEvent = ChangeEvent | RemoveEvent;
