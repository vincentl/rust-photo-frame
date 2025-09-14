#include <stdio.h>
#include <stdlib.h>
#include <jpeglib.h>

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "Usage: %s input.ppm output.jpg\n", argv[0]);
        return 1;
    }
    const char *input = argv[1];
    const char *output = argv[2];
    FILE *in = fopen(input, "rb");
    if (!in) { perror("fopen input"); return 1; }
    char magic[3];
    if (fscanf(in, "%2s", magic) != 1 || magic[0] != 'P' || magic[1] != '6') {
        fprintf(stderr, "Unsupported PPM format\n");
        fclose(in); return 1; }
    int width, height, maxval;
    if (fscanf(in, "%d %d %d", &width, &height, &maxval) != 3) {
        fprintf(stderr, "Bad PPM header\n");
        fclose(in); return 1; }
    fgetc(in); // consume single whitespace after header
    unsigned char *data = malloc(width * height * 3);
    if (!data) { fprintf(stderr, "Memory alloc failed\n"); fclose(in); return 1; }
    if (fread(data, 3, width * height, in) != (size_t)(width*height)) {
        fprintf(stderr, "Failed to read pixel data\n");
        free(data); fclose(in); return 1; }
    fclose(in);

    struct jpeg_compress_struct cinfo;
    struct jpeg_error_mgr jerr;
    cinfo.err = jpeg_std_error(&jerr);
    jpeg_create_compress(&cinfo);

    FILE *out = fopen(output, "wb");
    if (!out) { perror("fopen output"); free(data); return 1; }
    jpeg_stdio_dest(&cinfo, out);

    cinfo.image_width = width;
    cinfo.image_height = height;
    cinfo.input_components = 3;
    cinfo.in_color_space = JCS_RGB;

    jpeg_set_defaults(&cinfo);
    jpeg_set_quality(&cinfo, 85, TRUE);

    jpeg_start_compress(&cinfo, TRUE);
    JSAMPROW row_pointer;
    while (cinfo.next_scanline < cinfo.image_height) {
        row_pointer = &data[cinfo.next_scanline * width * 3];
        jpeg_write_scanlines(&cinfo, &row_pointer, 1);
    }
    jpeg_finish_compress(&cinfo);
    fclose(out);
    jpeg_destroy_compress(&cinfo);
    free(data);
    return 0;
}
