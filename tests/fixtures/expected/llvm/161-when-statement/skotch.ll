; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [9 x i8] c"negative\00", align 1
@.str.1 = private unnamed_addr constant [5 x i8] c"zero\00", align 1
@.str.2 = private unnamed_addr constant [6 x i8] c"small\00", align 1
@.str.3 = private unnamed_addr constant [6 x i8] c"large\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define void @InputKt_printCategory(i32 %arg0) {
entry:
  %merge_2 = alloca void
  %t0 = add i32 0, 1
  br label %bb1
bb1:
  %t1 = add i32 0, 0
  %t2 = icmp slt i32 %arg0, %t1
  br i1 %t2, label %bb2, label %bb3
bb2:
  call i32 @puts(ptr @.str.0)
  br label %bb8
bb3:
  %t4 = add i32 0, 0
  %t5 = icmp eq i32 %arg0, %t4
  br i1 %t5, label %bb4, label %bb5
bb4:
  call i32 @puts(ptr @.str.1)
  br label %bb8
bb5:
  %t7 = add i32 0, 10
  %t8 = icmp slt i32 %arg0, %t7
  br i1 %t8, label %bb6, label %bb7
bb6:
  call i32 @puts(ptr @.str.2)
  br label %bb8
bb7:
  call i32 @puts(ptr @.str.3)
  br label %bb8
bb8:
  ret void
}

define i32 @main() {
entry:
  %t0 = add i32 0, -5
  call void @InputKt_printCategory(i32 %t0)
  %t1 = add i32 0, 0
  call void @InputKt_printCategory(i32 %t1)
  %t2 = add i32 0, 7
  call void @InputKt_printCategory(i32 %t2)
  %t3 = add i32 0, 100
  call void @InputKt_printCategory(i32 %t3)
  ret i32 0
}

