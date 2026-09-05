export enum EditorEventType { Change = 0, FollowLink = 6, Remove = 8 }
export interface ChangeEvent { kind: EditorEventType.Change; value: string }
export interface FollowLinkEvent { kind: EditorEventType.FollowLink; link: string }
export interface RemoveEvent { kind: EditorEventType.Remove }
export type EditorEvent = ChangeEvent | FollowLinkEvent | RemoveEvent;
