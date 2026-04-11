; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [6 x i8] c"Hello\00", align 1
@.str.1 = private unnamed_addr constant [3 x i8] c", \00", align 1
@.str.2 = private unnamed_addr constant [7 x i8] c"world!\00", align 1
@.str.3 = private unnamed_addr constant [7 x i8] c"Kotlin\00", align 1
@.str.4 = private unnamed_addr constant [8 x i8] c"I love \00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)
@.fmt.concat.ss = private unnamed_addr constant [5 x i8] c"%s%s\00", align 1
@.fmt.concat.sd = private unnamed_addr constant [5 x i8] c"%s%d\00", align 1
declare i32 @snprintf(ptr, i32, ptr, ...)

define i32 @main() {
entry:
  %t0 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t0, i32 256, ptr @.fmt.concat.ss, ptr @.str.0, ptr @.str.1)
  %t2 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t2, i32 256, ptr @.fmt.concat.ss, ptr %t0, ptr @.str.2)
  call i32 @puts(ptr %t2)
  %t5 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t5, i32 256, ptr @.fmt.concat.ss, ptr @.str.4, ptr @.str.3)
  call i32 @puts(ptr %t5)
  ret i32 0
}

