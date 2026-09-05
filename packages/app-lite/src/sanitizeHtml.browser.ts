import DOMPurify from 'dompurify';

export default (html: string) => DOMPurify.sanitize(html);
