export const isResourceUrl = (url: string) => Boolean(url && url.length === 34 && url[0] === ':' && url[1] === '/');
